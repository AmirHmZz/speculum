use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use speculum::{
    App,
    app::Clock,
    cache::{self, ArtifactInput},
};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::{Layer, layer::SubscriberExt};

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Fixture {
    base: String,
    files: AtomicUsize,
    metadata: AtomicUsize,
    offline: AtomicBool,
    bad_hash: AtomicBool,
    malformed: AtomicBool,
    seen_auth: AtomicBool,
    download_gate: std::sync::Mutex<Option<Arc<DownloadGate>>>,
}
struct DownloadGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
async fn fixture(State(state): State<Arc<Fixture>>, request: Request) -> Response {
    if request.headers().contains_key("authorization") {
        state.seen_auth.store(true, Ordering::SeqCst);
    }
    if state.offline.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let path = request.uri().path();
    if path.starts_with("/files/") {
        if request.method() == "GET" {
            state.files.fetch_add(1, Ordering::SeqCst);
            let gate = state.download_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
        }
        let body = if state.bad_hash.load(Ordering::SeqCst) {
            "incorrect bytes"
        } else {
            "fixture archive"
        };
        return (
            [
                ("content-type", "application/octet-stream"),
                ("content-length", "15"),
            ],
            if request.method() == "HEAD" {
                Body::empty()
            } else {
                Body::from(body)
            },
        )
            .into_response();
    }
    state.metadata.fetch_add(1, Ordering::SeqCst);
    if state.malformed.load(Ordering::SeqCst) {
        return ([("content-type", "application/json")], "invalid JSON").into_response();
    }
    let hash = format!("{:x}", Sha256::digest(b"fixture archive"));
    let document = match path {
        "/simple/demo/" => json!({"meta":{"api-version":"1.0"},"name":"demo","files":[
            {"filename":"demo-1.0.0-py3-none-any.whl","url":format!("{}/files/demo.whl",state.base),"hashes":{"sha256":hash},"requires-python":">=3.9","core-metadata":{"sha256":hash},"dist-info-metadata":true},
            {"filename":"demo-2.0.0-py3-none-any.whl","url":format!("{}/files/other.whl",state.base),"hashes":{"sha512":format!("{:x}",sha2::Sha512::digest(b"fixture archive"))},"yanked":"fixture"}]}),
        "/npm/demo" | "/npm/@scope%2Fdemo" | "/npm/%40scope%2Fdemo" => {
            json!({"name":"demo","dist-tags":{"latest":"1.0.0"},"versions":{"1.0.0":{"name":"demo","version":"1.0.0","dist":{"tarball":format!("{}/files/demo.tgz",state.base),"shasum":format!("{:x}",sha1::Sha1::digest(b"fixture archive"))}}}})
        }
        "/npm/react%2Dresizable%2Dpanels"
        | "/npm/%40rolldown%2Fbinding%2Dlinux%2Dx64%2Dgnu"
        | "/npm/pre%2Ddemo" => {
            let (name, version) = match path {
                "/npm/react%2Dresizable%2Dpanels" => ("react-resizable-panels", "4.12.2"),
                "/npm/pre%2Ddemo" => ("pre-demo", "1.0.0-beta.1+build.7"),
                _ => ("@rolldown/binding-linux-x64-gnu", "1.2.3"),
            };
            json!({"name":name,"versions":{version:{"name":name,"version":version,"dist":{"tarball":format!("{}/files/demo.tgz",state.base),"integrity":format!("sha512-{}",STANDARD.encode(sha2::Sha512::digest(b"fixture archive")))}}}})
        }
        "/index/config.json" => {
            return (
                [("content-type", "application/octet-stream")],
                serde_json::to_vec_pretty(
                    &json!({"dl":format!("{}/files/{{crate}}-{{version}}.crate",state.base)}),
                )
                .unwrap(),
            )
                .into_response();
        }
        "/index/de/mo/demo" => {
            json!({"name":"demo","vers":"1.0.0","deps":[],"cksum":hash,"features":{},"yanked":false})
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let bytes = serde_json::to_vec(&document).unwrap();
    if path.contains("/index/de/") {
        (
            [
                ("content-type", "text/plain"),
                ("etag", "\"fixture-index\""),
            ],
            bytes,
        )
            .into_response()
    } else if request.headers().get("if-none-match").is_some() {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [
                ("content-type", "application/json"),
                ("etag", "\"fixture-meta\""),
            ],
            bytes,
        )
            .into_response()
    }
}
struct Harness {
    app: Arc<App>,
    clock: Arc<TestClock>,
    upstream: Arc<Fixture>,
    base: String,
    client: reqwest::Client,
    _dir: TempDir,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Harness {
    async fn new(auth_required: bool) -> Self {
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = Arc::new(Fixture {
            base: format!("http://{}", upstream_listener.local_addr().unwrap()),
            files: AtomicUsize::new(0),
            metadata: AtomicUsize::new(0),
            offline: AtomicBool::new(false),
            bad_hash: AtomicBool::new(false),
            malformed: AtomicBool::new(false),
            seen_auth: AtomicBool::new(false),
            download_gate: std::sync::Mutex::new(None),
        });
        let upstream_router = Router::new().fallback(fixture).with_state(upstream.clone());
        let upstream_task = tokio::spawn(
            async move {
                axum::serve(upstream_listener, upstream_router)
                    .await
                    .unwrap();
            }
            .with_current_subscriber(),
        );
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1700000000)));
        let app = App::open_with_clock(dir.path(), clock.clone())
            .await
            .unwrap();
        let hash = speculum::auth::hash_password("admin-password".into())
            .await
            .unwrap();
        sqlx::query("UPDATE admin SET password_hash=?,must_change_password=0 WHERE id=1")
            .bind(hash)
            .execute(&app.db)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut settings = app.snapshot().await.settings;
        settings.public_base_url = base.clone();
        for (kind, path) in [("pypi", "simple"), ("npm", "npm"), ("cargo", "index")] {
            let config = settings.registries.get_mut(kind).unwrap();
            config.upstream_url = format!("{}/{path}/", upstream.base);
            config.auth_required = auth_required;
        }
        app.save_settings(settings).await.unwrap();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let router = speculum::router(app.clone()).layer(axum::middleware::from_fn(
            move |request: Request, next: axum::middleware::Next| {
                next.run(request).with_subscriber(dispatch.clone())
            },
        ));
        let task = tokio::spawn(
            async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .unwrap();
            }
            .with_current_subscriber(),
        );
        Self {
            app,
            clock,
            upstream,
            base,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            _dir: dir,
            tasks: vec![task, upstream_task],
        }
    }
    async fn pypi(&self) -> Value {
        let response = self
            .client
            .get(format!("{}/pypi/simple/demo/", self.base))
            .header("accept", "application/vnd.pypi.simple.v1+json")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    }
    async fn login(&self) -> (String, String) {
        let response = self
            .client
            .post(format!("{}/api/admin/login", self.base))
            .json(&json!({"username":"admin","password":"admin-password"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body: Value = response.json().await.unwrap();
        (cookie, body["csrf_token"].as_str().unwrap().into())
    }
}
#[tokio::test]
async fn mirror_roots_redirect_without_trailing_slashes_and_preserve_authentication() {
    let h = Harness::new(false).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for (path, target) in [
        ("/npm", "/npm/"),
        ("/pypi", "/pypi/simple/"),
        ("/pypi/", "/pypi/simple/"),
        ("/pypi/simple", "/pypi/simple/"),
        ("/cargo", "/cargo/index/config.json"),
        ("/cargo/", "/cargo/index/config.json"),
        ("/cargo/index", "/cargo/index/config.json"),
        ("/cargo/index/", "/cargo/index/config.json"),
    ] {
        for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
            let response = client
                .request(method, format!("{}{path}?check=1", h.base))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 308, "{path}");
            assert_eq!(response.headers()["location"], format!("{target}?check=1"));
        }
        assert_eq!(
            h.client
                .get(format!("{}{path}", h.base))
                .send()
                .await
                .unwrap()
                .status(),
            200,
            "{path}"
        );
    }
    let protected = Harness::new(true).await;
    for path in ["/npm", "/pypi/simple", "/cargo/index"] {
        assert_eq!(
            client
                .get(format!("{}{path}", protected.base))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    let (_, _, token, _) = onboard_portal_user(&protected, "root-user").await;
    assert_eq!(
        protected
            .client
            .get(format!("{}/npm", protected.base))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let mut settings = h.app.snapshot().await.settings;
    settings.registries.get_mut("npm").unwrap().enabled = false;
    h.app.save_settings(settings).await.unwrap();
    assert_eq!(
        client
            .get(format!("{}/npm", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn shared_login_detects_roles_preserves_password_gates_and_limits_attempts() {
    let h = Harness::new(true).await;
    let (alice_id, temporary, _) = create_portal_user(&h, "alice").await;
    let response = h
        .client
        .post(format!("{}/api/login", h.base))
        .json(&json!({"username":"alice","password":temporary}))
        .send()
        .await
        .unwrap();
    let restricted = portal_session(response).await;
    assert!(restricted.must_change);
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/summary",
            reqwest::Method::GET,
            None,
            Some(&restricted)
        )
        .await
        .status(),
        403
    );
    // A mirror user may already have the same username as the administrator.
    onboard_portal_user(&h, "admin").await;
    for (username, password, role) in [
        ("admin", "admin-password", "admin"),
        ("admin", "admin-permanent-password", "user"),
        ("alice", temporary.as_str(), "user"),
    ] {
        let response = h
            .client
            .post(format!("{}/api/login", h.base))
            .json(&json!({"username":username,"password":password}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["role"], role);
        assert_eq!(
            h.client
                .get(format!("{}/api/{role}/session", h.base))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let other = if role == "admin" { "user" } else { "admin" };
        assert_eq!(
            h.client
                .get(format!("{}/api/{other}/session", h.base))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    sqlx::query("UPDATE users SET enabled=0 WHERE id=?")
        .bind(alice_id)
        .execute(&h.app.db)
        .await
        .unwrap();
    assert_eq!(
        h.client
            .post(format!("{}/api/login", h.base))
            .json(&json!({"username":"alice","password":temporary}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        h.client
            .post(format!("{}/api/login", h.base))
            .json(&json!({"username":"admin","password":"admin-password","role":"admin"}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    h.app.login_attempts.lock().unwrap().clear();
    for _ in 0..10 {
        assert_eq!(
            h.client
                .post(format!("{}/api/login", h.base))
                .json(&json!({"username":"missing","password":"wrong-password"}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    assert_eq!(
        h.client
            .post(format!("{}/api/login", h.base))
            .json(&json!({"username":"admin","password":"admin-password"}))
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    h.clock.0.fetch_add(300, Ordering::SeqCst);
    assert_eq!(
        h.client
            .post(format!("{}/api/login", h.base))
            .json(&json!({"username":"admin","password":"admin-password"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn token_last_used_tracks_valid_requests_and_cascades_on_revocation() {
    let h = Harness::new(true).await;
    let (id, session, token, token_id) = onboard_portal_user(&h, "usage-user").await;
    let unused: Value = portal_request(
        &h,
        "user",
        "/tokens",
        reqwest::Method::POST,
        Some(json!({"name":"unused"})),
        Some(&session),
    )
    .await
    .json()
    .await
    .unwrap();
    let list: Value = portal_request(
        &h,
        "user",
        "/tokens",
        reqwest::Method::GET,
        None,
        Some(&session),
    )
    .await
    .json()
    .await
    .unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|row| row["last_used_at"].is_null())
    );
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .basic_auth("wrong-user", Some(&token))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .basic_auth("usage-user", Some(&token))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    h.clock.0.fetch_add(60, Ordering::SeqCst);
    assert_eq!(
        h.client
            .head(format!("{}/npm/-/ping", h.base))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let used_at = h.app.now();
    let list: Value = portal_request(
        &h,
        "user",
        "/tokens",
        reqwest::Method::GET,
        None,
        Some(&session),
    )
    .await
    .json()
    .await
    .unwrap();
    for row in list.as_array().unwrap() {
        assert_eq!(
            row["last_used_at"],
            if row["id"] == token_id {
                json!(used_at)
            } else {
                Value::Null
            }
        );
    }
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == unused["id"])
    );
    sqlx::query("UPDATE users SET enabled=0 WHERE id=?")
        .bind(&id)
        .execute(&h.app.db)
        .await
        .unwrap();
    h.clock.0.fetch_add(60, Ordering::SeqCst);
    assert_eq!(
        h.client
            .get(format!("{}/npm/-/ping", h.base))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let stored: i64 = sqlx::query_scalar("SELECT last_used_at FROM token_usage WHERE token_id=?")
        .bind(&token_id)
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(stored, used_at);
    sqlx::query("UPDATE users SET enabled=1 WHERE id=?")
        .bind(&id)
        .execute(&h.app.db)
        .await
        .unwrap();
    assert_eq!(
        portal_request(
            &h,
            "user",
            &format!("/tokens/{token_id}"),
            reqwest::Method::DELETE,
            None,
            Some(&session)
        )
        .await
        .status(),
        204
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM token_usage WHERE token_id=?")
        .bind(&token_id)
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn token_usage_table_is_added_to_existing_databases_without_resetting_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    sqlx::query("INSERT INTO users(id,username,password_hash,created_at) VALUES('existing','existing','fixture',1)")
        .execute(&app.db).await.unwrap();
    sqlx::query("INSERT INTO tokens(id,user_id,name,prefix,hash,created_at) VALUES('existing-token','existing','existing','spm_fixture','fixture',1)")
        .execute(&app.db).await.unwrap();
    sqlx::query("DROP TABLE token_usage")
        .execute(&app.db)
        .await
        .unwrap();
    app.db.close().await;
    drop(app);
    let app = App::open(dir.path()).await.unwrap();
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM users),(SELECT COUNT(*) FROM tokens),(SELECT COUNT(*) FROM token_usage)",
    ).fetch_one(&app.db).await.unwrap();
    assert_eq!(counts, (1, 1, 0));
}

#[tokio::test]
async fn pypi_on_demand_head_ranges_checksums_and_statistics() {
    let h = Harness::new(false).await;
    let metadata = h.pypi().await;
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 0);
    assert_eq!(metadata["files"][0]["requires-python"], ">=3.9");
    assert_eq!(metadata["files"][1]["yanked"], "fixture");
    let url = metadata["files"][0]["url"].as_str().unwrap();
    assert_eq!(h.client.head(url).send().await.unwrap().status(), 200);
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 0);
    let (one, two) = tokio::join!(h.client.get(url).send(), h.client.get(url).send());
    assert_eq!(one.unwrap().bytes().await.unwrap(), "fixture archive");
    assert_eq!(two.unwrap().bytes().await.unwrap(), "fixture archive");
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 1);
    let partial = h
        .client
        .get(url)
        .header("range", "bytes=-7")
        .send()
        .await
        .unwrap();
    assert_eq!(partial.status(), 206);
    assert_eq!(partial.headers()["content-range"], "bytes 8-14/15");
    let etag = partial.headers()["etag"].clone();
    assert_eq!(partial.text().await.unwrap(), "archive");
    assert_eq!(
        h.client
            .get(url)
            .header("if-none-match", etag)
            .send()
            .await
            .unwrap()
            .status(),
        304
    );
    assert_eq!(
        h.client
            .get(url)
            .header("range", "bytes=99-")
            .send()
            .await
            .unwrap()
            .status(),
        416
    );
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE downloaded_at IS NOT NULL")
            .fetch_one(&h.app.db)
            .await
            .unwrap();
    assert_eq!(stored, 1);
    let html = h
        .client
        .get(format!("{}/pypi/simple/De_mo/", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(html.status(), 404);
    let html = h
        .client
        .get(format!("{}/pypi/simple/demo/", h.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(html.contains("data-core-metadata"));
    assert!(html.contains("#sha256="));
    assert!(html.contains("#sha512="));
    let sidecar = format!("{url}.metadata");
    assert_eq!(h.client.get(&sidecar).send().await.unwrap().status(), 200);
    assert!(!h.upstream.seen_auth.load(Ordering::SeqCst));
}
#[tokio::test]
async fn lifetime_changes_extend_existing_files_and_forever_survives_outage() {
    let h = Harness::new(false).await;
    let metadata = h.pypi().await;
    let url = metadata["files"][0]["url"].as_str().unwrap();
    let mut settings = h.app.snapshot().await.settings;
    settings
        .registries
        .get_mut("pypi")
        .unwrap()
        .cache_lifetime_seconds = Some(10);
    h.app.save_settings(settings.clone()).await.unwrap();
    assert_eq!(
        h.client
            .get(url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "fixture archive"
    );
    h.clock.0.fetch_add(9, Ordering::SeqCst);
    settings
        .registries
        .get_mut("pypi")
        .unwrap()
        .cache_lifetime_seconds = Some(100);
    h.app.save_settings(settings.clone()).await.unwrap();
    h.clock.0.fetch_add(10, Ordering::SeqCst);
    h.upstream.offline.store(true, Ordering::SeqCst);
    assert_eq!(h.client.get(url).send().await.unwrap().status(), 200);
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 1);
    settings
        .registries
        .get_mut("pypi")
        .unwrap()
        .metadata_freshness_seconds = 0;
    h.app.save_settings(settings.clone()).await.unwrap();
    assert_eq!(h.pypi().await["files"][0]["url"], url);
    let missing = metadata["files"][1]["url"].as_str().unwrap();
    assert_eq!(h.client.get(missing).send().await.unwrap().status(), 502);
    settings
        .registries
        .get_mut("pypi")
        .unwrap()
        .cache_lifetime_seconds = None;
    h.app.save_settings(settings.clone()).await.unwrap();
    h.clock.0.fetch_add(1000, Ordering::SeqCst);
    cache::cleanup(&h.app).await.unwrap();
    assert_eq!(h.client.get(url).send().await.unwrap().status(), 200);
    settings
        .registries
        .get_mut("pypi")
        .unwrap()
        .cache_lifetime_seconds = Some(10);
    h.app.save_settings(settings).await.unwrap();
    cache::cleanup(&h.app).await.unwrap();
    assert_eq!(h.client.get(url).send().await.unwrap().status(), 502);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE downloaded_at IS NOT NULL")
            .fetch_one(&h.app.db)
            .await
            .unwrap();
    assert_eq!(count, 0);
    h.upstream.offline.store(false, Ordering::SeqCst);
    assert_eq!(h.client.get(url).send().await.unwrap().status(), 200);
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn admin_csrf_users_tokens_and_independent_registry_authentication() {
    let h = Harness::new(true).await;
    assert_eq!(
        h.client
            .get(format!("{}/api/admin/settings", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/config.json", h.base))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["auth-required"],
        true
    );
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/de/mo/demo", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let (cookie, csrf) = h.login().await;
    let users = format!("{}/api/admin/users", h.base);
    let user = json!({"username":"developer","enabled":true});
    assert_eq!(
        h.client
            .post(&users)
            .header("cookie", &cookie)
            .json(&user)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let created: Value = h
        .client
        .post(&users)
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .json(&user)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();
    let password_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name='password_hash'",
    )
    .fetch_one(&h.app.db)
    .await
    .unwrap();
    assert_eq!(password_columns, 1);
    let response = h
        .client
        .get(format!("{}/pypi/simple/demo/", h.base))
        .basic_auth("developer", created["temporary_password"].as_str())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .basic_auth("admin", Some("admin-password"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let issued: Value = h
        .client
        .post(format!("{users}/{id}/tokens"))
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"name":"test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = issued["token"].as_str().unwrap();
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .basic_auth("developer", Some(token))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .basic_auth("other-user", Some(token))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let obsolete_password =
        json!({"username":"developer","enabled":true,"password":"unsupported-password"});
    for (method, url) in [
        (reqwest::Method::POST, users.clone()),
        (reqwest::Method::PUT, format!("{users}/{id}")),
    ] {
        assert_eq!(
            h.client
                .request(method, url)
                .header("cookie", &cookie)
                .header("x-csrf-token", &csrf)
                .json(&obsolete_password)
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    assert_eq!(
        h.client
            .get(format!("{}/npm/demo", h.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/de/mo/demo", h.base))
            .header("authorization", token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.client
            .get(format!("{}/api/admin/settings", h.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut settings = h.app.snapshot().await.settings;
    settings.registries.get_mut("npm").unwrap().auth_required = false;
    h.app.save_settings(settings).await.unwrap();
    assert_eq!(
        h.client
            .get(format!("{}/npm/demo", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let mut disabled = user.clone();
    disabled["enabled"] = json!(false);
    assert_eq!(
        h.client
            .put(format!("{users}/{id}"))
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .json(&disabled)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/de/mo/demo", h.base))
            .header("authorization", token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .basic_auth("developer", Some(token))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(!h.upstream.seen_auth.load(Ordering::SeqCst));
}
#[tokio::test]
async fn npm_standard_tarballs_resolve_cold_and_share_metadata_cache_artifacts() {
    let h = Harness::new(false).await;
    let cases = [
        ("demo", "demo-1.0.0.tgz", "1.0.0"),
        ("@scope/demo", "demo-1.0.0.tgz", "1.0.0"),
        ("@scope%2Fdemo", "demo-1.0.0.tgz", "1.0.0"),
        ("%40scope%2Fdemo", "demo-1.0.0.tgz", "1.0.0"),
        (
            "react-resizable-panels",
            "react-resizable-panels-4.12.2.tgz",
            "4.12.2",
        ),
        (
            "@rolldown/binding-linux-x64-gnu",
            "binding-linux-x64-gnu-1.2.3.tgz",
            "1.2.3",
        ),
        (
            "pre-demo",
            "pre-demo-1.0.0-beta.1+build.7.tgz",
            "1.0.0-beta.1+build.7",
        ),
    ];
    for (name, filename, version) in cases {
        let url = format!("{}/npm/{name}/-/{filename}", h.base);
        let before = h.upstream.files.load(Ordering::SeqCst);
        assert_eq!(h.client.head(&url).send().await.unwrap().status(), 200);
        assert_eq!(h.upstream.files.load(Ordering::SeqCst), before);
        let (one, two) = tokio::join!(h.client.get(&url).send(), h.client.get(&url).send());
        for response in [one.unwrap(), two.unwrap()] {
            assert_eq!(response.status(), 200, "{url}");
            assert_eq!(response.text().await.unwrap(), "fixture archive");
        }
        let metadata: Value = h
            .client
            .get(format!("{}/npm/{name}", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let rewritten = metadata["versions"][version]["dist"]["tarball"]
            .as_str()
            .unwrap();
        let partial = h
            .client
            .get(rewritten)
            .header("range", "bytes=-7")
            .send()
            .await
            .unwrap();
        assert_eq!(partial.status(), 206);
        assert_eq!(partial.text().await.unwrap(), "archive");
    }
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 5);
    let artifacts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifacts WHERE registry='npm'")
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(artifacts, 5);
    // Cached archives remain usable even if upstream metadata is no longer available.
    sqlx::query("DELETE FROM metadata")
        .execute(&h.app.db)
        .await
        .unwrap();
    h.upstream.offline.store(true, Ordering::SeqCst);
    for (name, filename, _) in cases {
        let response = h
            .client
            .get(format!("{}/npm/{name}/-/{filename}", h.base))
            .header("range", "bytes=0-6")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 206);
        assert_eq!(response.text().await.unwrap(), "fixture");
    }
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 5);
    assert!(!h.upstream.seen_auth.load(Ordering::SeqCst));
    let mut settings = h.app.snapshot().await.settings;
    settings.registries.get_mut("npm").unwrap().upstream_url =
        format!("{}/other/", h.upstream.base);
    h.app.save_settings(settings).await.unwrap();
    h.upstream.offline.store(false, Ordering::SeqCst);
    assert_eq!(
        h.client
            .get(format!("{}/npm/demo/-/demo-1.0.0.tgz", h.base))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
}

#[tokio::test]
async fn npm_standard_tarballs_validate_paths_versions_and_checksums() {
    let h = Harness::new(false).await;
    for path in [
        "demo/-/other-1.0.0.tgz",
        "demo/-/demo-.tgz",
        "demo/-/demo-1.0.0.zip",
        "demo/-/demo-1.0.0.tgz/extra",
        "demo/-/demo-1.0.0%2Fextra.tgz",
    ] {
        assert_eq!(
            h.client
                .get(format!("{}/npm/{path}", h.base))
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "{path}"
        );
    }
    assert_eq!(h.upstream.metadata.load(Ordering::SeqCst), 0);
    for path in ["demo/-/demo-9.0.0.tgz", "missing/-/missing-1.0.0.tgz"] {
        assert_eq!(
            h.client
                .get(format!("{}/npm/{path}", h.base))
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "{path}"
        );
    }
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 0);
    h.upstream.bad_hash.store(true, Ordering::SeqCst);
    let response = h
        .client
        .get(format!("{}/npm/demo/-/demo-1.0.0.tgz", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 502);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("Artifact checksum mismatch")
    );
    assert_eq!(
        std::fs::read_dir(h.app.dir.join("artifacts"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(std::fs::read_dir(h.app.dir.join("tmp")).unwrap().count(), 0);
    let protected = Harness::new(true).await;
    assert_eq!(
        protected
            .client
            .get(format!("{}/npm/demo/-/demo-1.0.0.tgz", protected.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(protected.upstream.metadata.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn npm_scopes_cargo_downloads_and_cache_deletion_preserve_totals() {
    let h = Harness::new(false).await;
    let npm: Value = h
        .client
        .get(format!("{}/npm/@scope%2Fdemo", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 0);
    let url = npm["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .unwrap();
    assert!(url.starts_with(&h.base));
    assert_eq!(
        h.client
            .get(url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "fixture archive"
    );
    assert_eq!(
        h.client
            .get(format!("{}/cargo/crates/demo/1.0.0/download", h.base))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "fixture archive"
    );
    let (cookie, csrf) = h.login().await;
    let before: Value = h
        .client
        .get(format!("{}/api/admin/summary", h.base))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before["total"]["files"], 2);
    assert_eq!(before["total"]["requests"], 2);
    assert_eq!(
        h.client
            .delete(format!("{}/api/admin/cache/npm", h.base))
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let after: Value = h
        .client
        .get(format!("{}/api/admin/summary", h.base))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["total"]["files"], 1);
    assert_eq!(after["total"]["requests"], 2);
}
#[tokio::test]
async fn checksum_failures_leave_no_published_or_temporary_files() {
    let h = Harness::new(false).await;
    let doc = h.pypi().await;
    h.upstream.bad_hash.store(true, Ordering::SeqCst);
    assert_eq!(
        h.client
            .get(doc["files"][0]["url"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    assert_eq!(
        h.client
            .get(format!(
                "{}.metadata",
                doc["files"][0]["url"].as_str().unwrap()
            ))
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    assert_eq!(std::fs::read_dir(h.app.dir.join("tmp")).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(h.app.dir.join("artifacts"))
            .unwrap()
            .count(),
        0
    );
}
#[tokio::test]
async fn settings_encryption_restart_single_instance_and_validation() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    assert!(App::open(dir.path()).await.is_err());
    let mut settings = app.snapshot().await.settings;
    settings.registries.get_mut("pypi").unwrap().proxy = Some(speculum::config::ProxySettings {
        url: "socks5h://localhost:1080".into(),
        username: Some("proxy-user".into()),
        password: Some("secret-proxy-password".into()),
    });
    settings
        .registries
        .get_mut("npm")
        .unwrap()
        .cache_lifetime_seconds = Some(120);
    app.save_settings(settings.clone()).await.unwrap();
    let stored: String = sqlx::query_scalar("SELECT value FROM settings")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(!stored.contains("secret-proxy-password"));
    assert_eq!(
        app.snapshot().await.settings.redacted().registries["pypi"]
            .proxy
            .as_ref()
            .unwrap()
            .password
            .as_deref(),
        Some("********")
    );
    let mut invalid = settings.clone();
    invalid
        .registries
        .get_mut("npm")
        .unwrap()
        .cache_lifetime_seconds = Some(0);
    assert!(app.save_settings(invalid).await.is_err());
    assert_eq!(
        app.snapshot().await.settings.registries["npm"].cache_lifetime_seconds,
        Some(120)
    );
    app.db.close().await;
    drop(app);
    let reopened = App::open(dir.path()).await.unwrap();
    assert_eq!(
        reopened.snapshot().await.settings.registries["npm"].cache_lifetime_seconds,
        Some(120)
    );
    let username: String = sqlx::query_scalar("SELECT username FROM admin")
        .fetch_one(&reopened.db)
        .await
        .unwrap();
    assert_eq!(username, "admin");
}
#[tokio::test]
async fn eviction_waits_for_reader_and_rechecks_extended_lifetime() {
    let h = Harness::new(false).await;
    let runtime = h.app.snapshot().await;
    let item = cache::register(
        &h.app,
        &runtime,
        ArtifactInput {
            registry: "npm",
            package: "demo",
            version: "1.0.0",
            filename: "demo.tgz",
            url: &format!("{}/files/demo.tgz", h.upstream.base),
            checksums: BTreeMap::new(),
            sidecar: false,
        },
    )
    .await
    .unwrap();
    std::fs::write(item.path(&h.app), b"fixture archive").unwrap();
    sqlx::query("UPDATE artifacts SET downloaded_at=?,size=15 WHERE id=?")
        .bind(h.app.now())
        .bind(&item.id)
        .execute(&h.app.db)
        .await
        .unwrap();
    let mut settings = runtime.settings;
    settings
        .registries
        .get_mut("npm")
        .unwrap()
        .cache_lifetime_seconds = Some(1);
    h.app.save_settings(settings.clone()).await.unwrap();
    h.clock.0.fetch_add(2, Ordering::SeqCst);
    let reader = h.app.artifact_lock(&item.id).read_owned().await;
    let app = h.app.clone();
    let id = item.id.clone();
    let evict = tokio::spawn(async move { cache::evict(&app, &id, true).await.unwrap() });
    tokio::task::yield_now().await;
    assert!(!evict.is_finished());
    settings
        .registries
        .get_mut("npm")
        .unwrap()
        .cache_lifetime_seconds = Some(100);
    h.app.save_settings(settings).await.unwrap();
    drop(reader);
    assert!(!evict.await.unwrap());
    assert!(item.path(&h.app).exists());
}
#[test]
fn byte_ranges_and_normalized_names() {
    let mut headers = HeaderMap::new();
    headers.insert("range", "bytes=4-".parse().unwrap());
    assert_eq!(cache::byte_range(&headers, 10).unwrap(), Some((4, 9)));
    headers.insert("range", "bytes=0-1,4-5".parse().unwrap());
    assert!(cache::byte_range(&headers, 10).is_err());
    assert_eq!(
        speculum::registries::pypi::normalize("Some_..Package").unwrap(),
        "some-package"
    );
    assert!(speculum::registries::pypi::normalize("../secret").is_err());
    assert_eq!(
        cache::sri_checksums(&format!(
            "sha256-{}",
            STANDARD.encode(Sha256::digest(b"demo"))
        ))["sha256"],
        format!("{:x}", Sha256::digest(b"demo"))
    );
}

#[tokio::test]
async fn token_revocation_username_change_and_user_deletion_take_effect() {
    let h = Harness::new(true).await;
    let (cookie, csrf) = h.login().await;
    let api = format!("{}/api/admin", h.base);
    let user = json!({"username":"developer","enabled":true});
    let created: Value = h
        .client
        .post(format!("{api}/users"))
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .json(&user)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();
    let token: Value = h
        .client
        .post(format!("{api}/users/{id}/tokens"))
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"name":"revocation"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let index = format!("{}/cargo/index/de/mo/demo", h.base);
    assert_eq!(
        h.client
            .get(&index)
            .header("authorization", token["token"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.client
            .delete(format!("{api}/tokens/{}", token["id"].as_str().unwrap()))
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        h.client
            .get(&index)
            .header("authorization", token["token"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let python = format!("{}/pypi/simple/demo/", h.base);
    assert_eq!(
        h.client
            .get(&python)
            .basic_auth("developer", Some(token["token"].as_str().unwrap()))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let replacement: Value = h
        .client
        .post(format!("{api}/users/{id}/tokens"))
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"name":"cascade"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let replacement = replacement["token"].as_str().unwrap();
    let renamed = json!({"username":"renamed","enabled":true});
    assert_eq!(
        h.client
            .put(format!("{api}/users/{id}"))
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .json(&renamed)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        h.client
            .get(&python)
            .basic_auth("developer", Some(replacement))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        h.client
            .get(&python)
            .basic_auth("renamed", Some(replacement))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.client
            .get(&index)
            .header("authorization", replacement)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.client
            .delete(format!("{api}/users/{id}"))
            .header("cookie", &cookie)
            .header("x-csrf-token", &csrf)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tokens WHERE user_id=?")
        .bind(id)
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(
        h.client
            .get(&python)
            .basic_auth("renamed", Some(replacement))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

#[tokio::test]
async fn sessions_expire_and_admin_credential_changes_invalidate_all_sessions() {
    let h = Harness::new(false).await;
    let (cookie, _) = h.login().await;
    let mut settings = h.app.snapshot().await.settings;
    settings.public_base_url = "https://mirror.example.com".into();
    settings.admin_session_lifetime_seconds = 60;
    h.app.save_settings(settings).await.unwrap();
    let me = format!("{}/api/admin/session", h.base);
    let response = h
        .client
        .get(&me)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
    h.clock.0.fetch_add(60, Ordering::SeqCst);
    assert_eq!(
        h.client
            .get(&me)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let (cookie, csrf) = h.login().await;
    let response = h
        .client
        .put(format!("{}/api/admin/credentials", h.base))
        .header("cookie", &cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({"current_password":"admin-password","password":"updated-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        h.client
            .get(&me)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        h.client
            .post(format!("{}/api/admin/login", h.base))
            .json(&json!({"username":"admin","password":"updated-password"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn invalid_metadata_refresh_preserves_the_previous_valid_response() {
    let h = Harness::new(false).await;
    let initial = h.pypi().await;
    let mut settings = h.app.snapshot().await.settings;
    settings
        .registries
        .get_mut("pypi")
        .unwrap()
        .metadata_freshness_seconds = 0;
    h.app.save_settings(settings).await.unwrap();
    h.upstream.malformed.store(true, Ordering::SeqCst);
    let refreshed = h.pypi().await;
    assert_eq!(initial, refreshed);
}

#[tokio::test]
async fn socks5h_proxy_authentication_and_remote_dns_are_used() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let h = Harness::new(false).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = listener.local_addr().unwrap();
    let target = reqwest::Url::parse(&h.upstream.base)
        .unwrap()
        .port()
        .unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 2];
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting[0], 5);
        let mut methods = vec![0; greeting[1] as usize];
        stream.read_exact(&mut methods).await.unwrap();
        assert!(methods.contains(&2));
        stream.write_all(&[5, 2]).await.unwrap();
        let mut login = [0; 2];
        stream.read_exact(&mut login).await.unwrap();
        assert_eq!(login[0], 1);
        let mut user = vec![0; login[1] as usize];
        stream.read_exact(&mut user).await.unwrap();
        assert_eq!(user, b"proxy-user");
        let length = stream.read_u8().await.unwrap();
        let mut password = vec![0; length as usize];
        stream.read_exact(&mut password).await.unwrap();
        assert_eq!(password, b"proxy-password");
        stream.write_all(&[1, 0]).await.unwrap();
        let mut connect = [0; 4];
        stream.read_exact(&mut connect).await.unwrap();
        assert_eq!(connect, [5, 1, 0, 3]);
        let length = stream.read_u8().await.unwrap();
        let mut host = vec![0; length as usize];
        stream.read_exact(&mut host).await.unwrap();
        assert_eq!(host, b"fixture.invalid");
        assert_eq!(stream.read_u16().await.unwrap(), target);
        let mut upstream = tokio::net::TcpStream::connect(("127.0.0.1", target))
            .await
            .unwrap();
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
    });
    let mut settings = h.app.snapshot().await.settings;
    let config = settings.registries.get_mut("pypi").unwrap();
    config.upstream_url = format!("http://fixture.invalid:{target}/simple/");
    config.allowed_artifact_hosts.push("127.0.0.1".into());
    config.proxy = Some(speculum::config::ProxySettings {
        url: format!("socks5h://{proxy}"),
        username: Some("proxy-user".into()),
        password: Some("proxy-password".into()),
    });
    h.app.save_settings(settings).await.unwrap();
    assert_eq!(h.pypi().await["name"], "demo");
    server.abort();
}

#[tokio::test]
async fn changing_concurrency_reuses_limiter_and_observes_inflight_downloads() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    let runtime = app.snapshot().await;
    let first = runtime.downloads.acquire().await;
    let second = runtime.downloads.acquire().await;
    let mut settings = runtime.settings;
    settings.download_concurrency = 1;
    app.save_settings(settings).await.unwrap();
    let current = app.snapshot().await;
    assert!(Arc::ptr_eq(&runtime.downloads, &current.downloads));
    let limiter = current.downloads.clone();
    let waiting = tokio::spawn(async move { limiter.acquire().await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    drop(first);
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    drop(second);
    let permit = waiting.await.unwrap();
    drop(permit);
}

#[tokio::test]
async fn artifact_and_package_counters_survive_reopening_database() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    let runtime = app.snapshot().await;
    let item = cache::register(
        &app,
        &runtime,
        ArtifactInput {
            registry: "npm",
            package: "restart-test",
            version: "1.0.0",
            filename: "restart.tgz",
            url: "https://registry.npmjs.org/restart-test/-/restart.tgz",
            checksums: BTreeMap::new(),
            sidecar: false,
        },
    )
    .await
    .unwrap();
    std::fs::write(item.path(&app), b"persistent artifact").unwrap();
    sqlx::query("UPDATE artifacts SET downloaded_at=?,size=19 WHERE id=?")
        .bind(app.now())
        .bind(&item.id)
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE packages SET requests=7,hits=5,upstream_bytes=19 WHERE id=?")
        .bind(&item.package_id)
        .execute(&app.db)
        .await
        .unwrap();
    app.db.close().await;
    drop(app);
    let app = App::open(dir.path()).await.unwrap();
    let restored = cache::artifact(&app, &item.id).await.unwrap();
    assert!(restored.valid(&app.snapshot().await, app.now()));
    assert_eq!(
        std::fs::read(restored.path(&app)).unwrap(),
        b"persistent artifact"
    );
    let requests: i64 = sqlx::query_scalar("SELECT requests FROM packages WHERE id=?")
        .bind(item.package_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(requests, 7);
}

struct PortalSession {
    cookie: String,
    csrf: String,
    must_change: bool,
}
async fn portal_session(response: reqwest::Response) -> PortalSession {
    assert_eq!(response.status(), 200);
    let raw = response.headers()["set-cookie"].to_str().unwrap();
    assert!(raw.contains("HttpOnly") && raw.contains("SameSite=Strict"));
    let cookie = raw.split(';').next().unwrap().to_owned();
    let body: Value = response.json().await.unwrap();
    PortalSession {
        cookie,
        csrf: body["csrf_token"].as_str().unwrap().into(),
        must_change: body["must_change_password"].as_bool().unwrap(),
    }
}
async fn portal_request(
    h: &Harness,
    role: &str,
    path: &str,
    method: reqwest::Method,
    body: Option<Value>,
    session: Option<&PortalSession>,
) -> reqwest::Response {
    let mut request = h
        .client
        .request(method, format!("{}/api/{role}{path}", h.base));
    if let Some(session) = session {
        request = request
            .header("cookie", &session.cookie)
            .header("x-csrf-token", &session.csrf);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.send().await.unwrap()
}
async fn portal_login(h: &Harness, role: &str, username: &str, password: &str) -> PortalSession {
    portal_session(
        portal_request(
            h,
            role,
            "/login",
            reqwest::Method::POST,
            Some(json!({"username":username,"password":password})),
            None,
        )
        .await,
    )
    .await
}
async fn create_portal_user(h: &Harness, username: &str) -> (String, String, PortalSession) {
    let (cookie, csrf) = h.login().await;
    let admin = PortalSession {
        cookie,
        csrf,
        must_change: false,
    };
    let response = portal_request(
        h,
        "admin",
        "/users",
        reqwest::Method::POST,
        Some(json!({"username":username,"enabled":true})),
        Some(&admin),
    )
    .await;
    assert_eq!(response.status(), 201);
    let body: Value = response.json().await.unwrap();
    (
        body["id"].as_str().unwrap().into(),
        body["temporary_password"].as_str().unwrap().into(),
        admin,
    )
}
async fn onboard_portal_user(
    h: &Harness,
    username: &str,
) -> (String, PortalSession, String, String) {
    let (id, temporary, _) = create_portal_user(h, username).await;
    let restricted = portal_login(h, "user", username, &temporary).await;
    assert!(restricted.must_change);
    let password = format!("{username}-permanent-password");
    let session = portal_session(
        portal_request(
            h,
            "user",
            "/password",
            reqwest::Method::PUT,
            Some(json!({"current_password":temporary,"password":password})),
            Some(&restricted),
        )
        .await,
    )
    .await;
    assert!(!session.must_change);
    let token: Value = portal_request(
        h,
        "user",
        "/tokens",
        reqwest::Method::POST,
        Some(json!({"name":"client"})),
        Some(&session),
    )
    .await
    .json()
    .await
    .unwrap();
    (
        id,
        session,
        token["token"].as_str().unwrap().into(),
        token["id"].as_str().unwrap().into(),
    )
}
async fn personal(h: &Harness, session: &PortalSession) -> Value {
    tokio::task::yield_now().await;
    h.app.deliveries.close();
    h.app.deliveries.wait().await;
    let response = portal_request(
        h,
        "user",
        "/summary",
        reqwest::Method::GET,
        None,
        Some(session),
    )
    .await;
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
#[tokio::test]
async fn first_login_gates_both_roles_and_passwords_cannot_download_packages() {
    let h = Harness::new(true).await;
    sqlx::query("UPDATE admin SET must_change_password=1")
        .execute(&h.app.db)
        .await
        .unwrap();
    let restricted = portal_login(&h, "admin", "admin", "admin-password").await;
    assert!(restricted.must_change);
    for path in ["/settings", "/summary", "/users"] {
        assert_eq!(
            portal_request(
                &h,
                "admin",
                path,
                reqwest::Method::GET,
                None,
                Some(&restricted)
            )
            .await
            .status(),
            403
        );
    }
    assert_eq!(
        h.client
            .put(format!("{}/api/admin/credentials", h.base))
            .header("cookie", &restricted.cookie)
            .json(&json!({"current_password":"admin-password","password":"changed-admin-password"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(portal_request(&h,"admin","/credentials",reqwest::Method::PUT,Some(json!({"username":"owner","current_password":"admin-password","password":"changed-admin-password"})),Some(&restricted)).await.status(),422);
    let admin = portal_session(
        portal_request(
            &h,
            "admin",
            "/credentials",
            reqwest::Method::PUT,
            Some(json!({"current_password":"admin-password","password":"changed-admin-password"})),
            Some(&restricted),
        )
        .await,
    )
    .await;
    assert!(!admin.must_change);
    assert_eq!(
        portal_request(
            &h,
            "admin",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&restricted)
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&admin)
        )
        .await
        .status(),
        401
    );
    let created: Value = portal_request(
        &h,
        "admin",
        "/users",
        reqwest::Method::POST,
        Some(json!({"username":"alice","enabled":true})),
        Some(&admin),
    )
    .await
    .json()
    .await
    .unwrap();
    let temporary = created["temporary_password"].as_str().unwrap();
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=?")
        .bind(created["id"].as_str())
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$") && !hash.contains(temporary));
    assert!(speculum::auth::verify_password(temporary.into(), hash).await);
    let restricted = portal_login(&h, "user", "alice", temporary).await;
    assert!(restricted.must_change);
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&restricted)
        )
        .await
        .status(),
        200
    );
    for path in ["/summary", "/packages", "/config", "/tokens"] {
        assert_eq!(
            portal_request(
                &h,
                "user",
                path,
                reqwest::Method::GET,
                None,
                Some(&restricted)
            )
            .await
            .status(),
            403
        );
    }
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/tokens",
            reqwest::Method::POST,
            Some(json!({"name":"forbidden"})),
            Some(&restricted)
        )
        .await
        .status(),
        403
    );
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/password",
            reqwest::Method::PUT,
            Some(json!({"current_password":"wrong-password","password":"alice-new-password"})),
            Some(&restricted)
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/password",
            reqwest::Method::PUT,
            Some(json!({"current_password":temporary,"password":"short"})),
            Some(&restricted)
        )
        .await
        .status(),
        400
    );
    let user = portal_session(
        portal_request(
            &h,
            "user",
            "/password",
            reqwest::Method::PUT,
            Some(json!({"current_password":temporary,"password":"alice-new-password"})),
            Some(&restricted),
        )
        .await,
    )
    .await;
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&restricted)
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        portal_request(
            &h,
            "admin",
            "/users",
            reqwest::Method::GET,
            None,
            Some(&user)
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        h.client
            .get(format!("{}/pypi/simple/demo/", h.base))
            .basic_auth("alice", Some("alice-new-password"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/de/mo/demo", h.base))
            .header("cookie", &user.cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/tokens",
            reqwest::Method::POST,
            Some(json!({"name":"bad","user_id":"another"})),
            Some(&user)
        )
        .await
        .status(),
        422
    );
    let response = portal_request(
        &h,
        "user",
        "/config",
        reqwest::Method::GET,
        None,
        Some(&user),
    )
    .await;
    let config: Value = response.json().await.unwrap();
    assert_eq!(config.as_object().unwrap().len(), 2);
    assert_eq!(config["registries"]["pypi"].as_object().unwrap().len(), 2);
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::DELETE,
            None,
            Some(&user)
        )
        .await
        .status(),
        204
    );
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&user)
        )
        .await
        .status(),
        401
    );
}
#[tokio::test]
async fn user_tokens_sessions_resets_disabling_and_expiry_are_isolated() {
    let h = Harness::new(true).await;
    let (alice_id, alice, token, token_id) = onboard_portal_user(&h, "alice").await;
    let (_, bob, _, _) = onboard_portal_user(&h, "bob").await;
    assert_eq!(
        portal_request(
            &h,
            "user",
            &format!("/tokens/{token_id}"),
            reqwest::Method::DELETE,
            None,
            Some(&bob)
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        h.client
            .post(format!("{}/api/user/tokens", h.base))
            .header("cookie", &alice.cookie)
            .json(&json!({"name":"no-csrf"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let list: Value = portal_request(
        &h,
        "user",
        "/tokens",
        reqwest::Method::GET,
        None,
        Some(&alice),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["prefix"], &token[..12]);
    assert!(list[0].get("token").is_none() && list[0].get("hash").is_none());
    let second = portal_login(&h, "user", "alice", "alice-permanent-password").await;
    let fresh=portal_session(portal_request(&h,"user","/password",reqwest::Method::PUT,Some(json!({"current_password":"alice-permanent-password","password":"alice-updated-password"})),Some(&alice)).await).await;
    for old in [&alice, &second] {
        assert_eq!(
            portal_request(
                &h,
                "user",
                "/session",
                reqwest::Method::GET,
                None,
                Some(old)
            )
            .await
            .status(),
            401
        );
    }
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/de/mo/demo", h.base))
            .header("authorization", &token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let (cookie, csrf) = h.login().await;
    let admin = PortalSession {
        cookie,
        csrf,
        must_change: false,
    };
    let reset: Value = portal_request(
        &h,
        "admin",
        &format!("/users/{alice_id}/reset-password"),
        reqwest::Method::POST,
        None,
        Some(&admin),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&fresh)
        )
        .await
        .status(),
        401
    );
    let restricted = portal_login(
        &h,
        "user",
        "alice",
        reset["temporary_password"].as_str().unwrap(),
    )
    .await;
    assert!(restricted.must_change);
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/de/mo/demo", h.base))
            .header("authorization", &token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        portal_request(
            &h,
            "admin",
            &format!("/users/{alice_id}"),
            reqwest::Method::PUT,
            Some(json!({"username":"alice","enabled":false})),
            Some(&admin)
        )
        .await
        .status(),
        204
    );
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&restricted)
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        h.client
            .get(format!("{}/cargo/index/de/mo/demo", h.base))
            .header("authorization", &token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    portal_request(
        &h,
        "admin",
        &format!("/users/{alice_id}"),
        reqwest::Method::PUT,
        Some(json!({"username":"alice","enabled":true})),
        Some(&admin),
    )
    .await;
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&restricted)
        )
        .await
        .status(),
        401
    );
    let mut settings = h.app.snapshot().await.settings;
    settings.user_session_lifetime_seconds = 60;
    h.app.save_settings(settings).await.unwrap();
    h.clock.0.fetch_add(60, Ordering::SeqCst);
    assert_eq!(
        portal_request(
            &h,
            "user",
            "/session",
            reqwest::Method::GET,
            None,
            Some(&bob)
        )
        .await
        .status(),
        401
    );
    cache::cleanup(&h.app).await.unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions")
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    for attempt in 0..11 {
        let response = portal_request(
            &h,
            "user",
            "/login",
            reqwest::Method::POST,
            Some(json!({"username":"unknown","password":"not-the-password"})),
            None,
        )
        .await;
        assert_eq!(response.status(), if attempt < 10 { 401 } else { 429 });
    }
    h.clock.0.fetch_add(300, Ordering::SeqCst);
    assert!(
        !portal_login(&h, "user", "bob", "bob-permanent-password")
            .await
            .must_change
    );
}
#[tokio::test]
async fn personal_counters_ranges_failures_and_public_authentication() {
    let h = Harness::new(false).await;
    let (_, alice, alpha, alpha_id) = onboard_portal_user(&h, "alice").await;
    let (_, bob, beta, _) = onboard_portal_user(&h, "bob").await;
    let metadata = h.pypi().await;
    let url = metadata["files"][0]["url"].as_str().unwrap();
    assert_eq!(
        h.client
            .head(url)
            .bearer_auth(&alpha)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(personal(&h, &alice).await["total"]["requests"], 0);
    let response = h
        .client
        .get(url)
        .basic_auth("alice", Some(&alpha))
        .send()
        .await
        .unwrap();
    let etag = response.headers()["etag"].clone();
    assert_eq!(response.bytes().await.unwrap().len(), 15);
    assert_eq!(
        h.client
            .get(url)
            .bearer_auth(&beta)
            .header("range", "bytes=0-2")
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        h.client
            .get(url)
            .bearer_auth(&alpha)
            .header("if-none-match", etag)
            .send()
            .await
            .unwrap()
            .status(),
        304
    );
    h.client
        .get(url)
        .bearer_auth("invalid-public-credential")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    h.client.head(url).bearer_auth(&alpha).send().await.unwrap();
    let alice_stats = personal(&h, &alice).await;
    let bob_stats = personal(&h, &bob).await;
    assert_eq!(alice_stats["total"]["requests"], 2);
    assert_eq!(alice_stats["total"]["misses"], 1);
    assert_eq!(alice_stats["total"]["hits"], 1);
    assert_eq!(alice_stats["total"]["upstream_downloads"], 1);
    assert_eq!(alice_stats["total"]["client_bytes"], 15);
    assert_eq!(bob_stats["total"]["requests"], 1);
    assert_eq!(bob_stats["total"]["client_bytes"], 3);
    assert_eq!(bob_stats["total"]["upstream_downloads"], 0);
    assert!(alice_stats["total"].get("stored_bytes").is_none());
    h.upstream.bad_hash.store(true, Ordering::SeqCst);
    let other = metadata["files"][1]["url"].as_str().unwrap();
    assert_eq!(
        h.client
            .get(other)
            .bearer_auth(&beta)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    let failed = personal(&h, &bob).await;
    assert_eq!(failed["total"]["failures"], 1);
    assert_eq!(failed["total"]["upstream_bytes"], 15);
    assert_eq!(personal(&h, &alice).await["total"]["failures"], 0);
    let packages: Value = portal_request(
        &h,
        "user",
        "/packages?q=demo&registry=pypi&page_size=1",
        reqwest::Method::GET,
        None,
        Some(&alice),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(packages["total"], 1);
    assert_eq!(packages["items"][0]["requests"], 2);
    let missing: Value = portal_request(
        &h,
        "user",
        "/packages?registry=npm",
        reqwest::Method::GET,
        None,
        Some(&alice),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(missing["total"], 0);
    assert_eq!(
        portal_request(
            &h,
            "user",
            &format!("/tokens/{alpha_id}"),
            reqwest::Method::DELETE,
            None,
            Some(&alice)
        )
        .await
        .status(),
        204
    );
    assert_eq!(personal(&h, &alice).await["total"]["requests"], 2);
    let (cookie, csrf) = h.login().await;
    let admin = PortalSession {
        cookie,
        csrf,
        must_change: false,
    };
    portal_request(
        &h,
        "admin",
        "/cache/pypi",
        reqwest::Method::DELETE,
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(personal(&h, &alice).await["total"]["requests"], 2);
    let total: Value = portal_request(
        &h,
        "admin",
        "/summary",
        reqwest::Method::GET,
        None,
        Some(&admin),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(total["total"]["requests"], 5);
    assert_eq!(total["total"]["client_bytes"], 33);
}
#[tokio::test]
async fn concurrent_user_downloads_and_deletion_preserve_global_accounting() {
    let h = Harness::new(false).await;
    let (alice_id, _, alpha, _) = onboard_portal_user(&h, "alice").await;
    let (_, bob, beta, _) = onboard_portal_user(&h, "bob").await;
    let url = h.pypi().await["files"][0]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    let gate = Arc::new(DownloadGate {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    *h.upstream.download_gate.lock().unwrap() = Some(gate.clone());
    let first = {
        let client = h.client.clone();
        let url = url.clone();
        tokio::spawn(async move {
            client
                .get(url)
                .bearer_auth(alpha)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        })
    };
    gate.entered.notified().await;
    let second = {
        let client = h.client.clone();
        let url = url.clone();
        tokio::spawn(async move {
            client
                .get(url)
                .bearer_auth(beta)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        })
    };
    let (cookie, csrf) = h.login().await;
    let admin = PortalSession {
        cookie,
        csrf,
        must_change: false,
    };
    assert_eq!(
        portal_request(
            &h,
            "admin",
            &format!("/users/{alice_id}"),
            reqwest::Method::DELETE,
            None,
            Some(&admin)
        )
        .await
        .status(),
        204
    );
    gate.release.notify_one();
    assert_eq!(first.await.unwrap().len(), 15);
    assert_eq!(second.await.unwrap().len(), 15);
    let stats = personal(&h, &bob).await;
    assert_eq!(stats["total"]["requests"], 1);
    assert_eq!(stats["total"]["upstream_downloads"], 0);
    assert_eq!(h.upstream.files.load(Ordering::SeqCst), 1);
    let total: Value = portal_request(
        &h,
        "admin",
        "/summary",
        reqwest::Method::GET,
        None,
        Some(&admin),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(total["total"]["requests"], 2);
    assert_eq!(total["total"]["client_bytes"], 30);
    assert_eq!(total["total"]["upstream_downloads"], 1);
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_package_stats WHERE user_id=?")
            .bind(alice_id)
            .fetch_one(&h.app.db)
            .await
            .unwrap();
    assert_eq!(remaining, 0);
}
#[tokio::test]
async fn personal_history_and_password_flags_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    let initial_hash: String = sqlx::query_scalar("SELECT password_hash FROM admin")
        .fetch_one(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,username,password_hash,created_at) VALUES('user','alice','fixture-hash',?)").bind(app.now()).execute(&app.db).await.unwrap();
    let package = app.package("npm", "history").await.unwrap();
    speculum::stats::record(
        &app,
        &package,
        Some("user"),
        speculum::stats::Delta {
            requests: 2,
            hits: 1,
            misses: 1,
            client_bytes: 7,
            last_access: Some(app.now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    app.db.close().await;
    drop(app);
    let app = App::open(dir.path()).await.unwrap();
    assert_eq!(
        speculum::stats::summary(&app, "user").await.unwrap()["total"]["requests"],
        2
    );
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM admin")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(hash, initial_hash);
    let flag: bool = sqlx::query_scalar("SELECT must_change_password FROM users WHERE id='user'")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(flag);
    sqlx::query("DELETE FROM users WHERE id='user'")
        .execute(&app.db)
        .await
        .unwrap();
    speculum::stats::record(
        &app,
        &package,
        Some("user"),
        speculum::stats::Delta {
            client_bytes: 5,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let bytes: i64 = sqlx::query_scalar("SELECT client_bytes FROM packages WHERE id=?")
        .bind(package)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(bytes, 12);
    assert_eq!(
        speculum::stats::summary(&app, "user").await.unwrap()["total"]["requests"],
        0
    );
}

#[tokio::test]
async fn token_prefixes_from_both_creation_paths_survive_restart_and_remain_private() {
    use axum::{
        Extension, Json,
        extract::{Path, State},
    };
    use speculum::{admin::TokenInput, auth::DashboardSession};
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    for name in ["alice", "bob"] {
        sqlx::query("INSERT INTO users(id,username,password_hash,must_change_password,created_at) VALUES(?,?, 'fixture',0,?)")
            .bind(name).bind(name).bind(app.now()).execute(&app.db).await.unwrap();
    }
    sqlx::query(
        "INSERT INTO user_sessions(id,user_id,csrf,created_at) VALUES('session','alice','csrf',?)",
    )
    .bind(app.now())
    .execute(&app.db)
    .await
    .unwrap();
    let alice = DashboardSession {
        id: "session".into(),
        user_id: Some("alice".into()),
        username: "alice".into(),
        csrf: "csrf".into(),
        must_change_password: false,
    };
    let bob = DashboardSession {
        user_id: Some("bob".into()),
        username: "bob".into(),
        ..alice.clone()
    };
    let user_token = speculum::user::create_token(
        State(app.clone()),
        Extension(alice.clone()),
        Json(TokenInput {
            name: "user-issued".into(),
        }),
    )
    .await
    .unwrap()
    .0;
    let admin_token = speculum::admin::create_token(
        State(app.clone()),
        Path("alice".into()),
        Json(TokenInput {
            name: "admin-issued".into(),
        }),
    )
    .await
    .unwrap()
    .0;
    for issued in [&user_token, &admin_token] {
        let value = issued["token"].as_str().unwrap();
        let (prefix, hash): (String, String) =
            sqlx::query_as("SELECT prefix,hash FROM tokens WHERE id=?")
                .bind(issued["id"].as_str().unwrap())
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(prefix, &value[..12]);
        assert_eq!(prefix.len(), 12);
        assert!(prefix.starts_with("spm_"));
        assert_eq!(hash, speculum::auth::digest(value));
        assert_ne!(hash, value);
    }
    assert_eq!(
        speculum::user::tokens(State(app.clone()), Extension(bob.clone()))
            .await
            .unwrap()
            .0,
        json!([])
    );
    let error = speculum::user::delete_token(
        State(app.clone()),
        Extension(bob),
        Path(user_token["id"].as_str().unwrap().into()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::NOT_FOUND);
    sqlx::query("INSERT INTO token_usage(token_id,last_used_at) VALUES(?,1700000000)")
        .bind(user_token["id"].as_str().unwrap())
        .execute(&app.db)
        .await
        .unwrap();
    app.db.close().await;
    drop(app);
    let app = App::open(dir.path()).await.unwrap();
    let listed = speculum::user::tokens(State(app.clone()), Extension(alice))
        .await
        .unwrap()
        .0;
    assert_eq!(listed.as_array().unwrap().len(), 2);
    for issued in [&user_token, &admin_token] {
        let row = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == issued["id"])
            .unwrap();
        assert_eq!(row["prefix"], &issued["token"].as_str().unwrap()[..12]);
        assert_eq!(
            row["last_used_at"],
            if row["id"] == user_token["id"] {
                json!(1700000000)
            } else {
                Value::Null
            }
        );
        assert!(row.get("token").is_none() && row.get("hash").is_none());
    }
}

#[derive(Clone, Default)]
struct LogCapture(Arc<std::sync::Mutex<Vec<CapturedLog>>>);
fn log_subscriber(capture: LogCapture, filter: &str) -> impl tracing::Subscriber + Send + Sync {
    // Keep a second dispatcher alive: tracing's single-dispatcher optimization otherwise
    // caches a callsite's interest from unrelated test threads with no subscriber.
    static SENTINEL: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    SENTINEL.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    tracing_subscriber::registry()
        .with(capture)
        .with(tracing_subscriber::EnvFilter::new(filter))
}
#[derive(Debug)]
struct CapturedLog {
    level: tracing::Level,
    fields: BTreeMap<String, String>,
}
#[derive(Default)]
struct LogFields(BTreeMap<String, String>);
impl tracing::field::Visit for LogFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
}
impl<S: tracing::Subscriber> Layer<S> for LogCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = LogFields::default();
        event.record(&mut fields);
        self.0.lock().unwrap().push(CapturedLog {
            level: *event.metadata().level(),
            fields: fields.0,
        });
    }
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = LogFields::default();
        attrs.record(&mut fields);
        self.0.lock().unwrap().push(CapturedLog {
            level: *attrs.metadata().level(),
            fields: fields.0,
        });
    }
}
impl LogCapture {
    fn take(&self) -> Vec<CapturedLog> {
        let logs = std::mem::take(&mut *self.0.lock().unwrap());
        let rendered = format!("{logs:?}");
        for secret in [
            "client-query-secret",
            "upstream-query-secret",
            "header-secret",
            "body-secret",
            "proxy-secret",
        ] {
            assert!(!rendered.contains(secret), "Credential leaked: {secret}");
        }
        logs
    }
}
fn logged(
    logs: &[CapturedLog],
    level: tracing::Level,
    message: &str,
    fields: &[(&str, &str)],
) -> bool {
    logs.iter().any(|log| {
        log.level == level
            && log
                .fields
                .get("message")
                .is_some_and(|value| value == message)
            && fields
                .iter()
                .all(|(key, value)| log.fields.get(*key).is_some_and(|field| field == value))
    })
}

#[tokio::test]
async fn registry_logging_covers_cache_refresh_failures_and_credentials() {
    let capture = LogCapture::default();
    let subscriber = log_subscriber(capture.clone(), "speculum=debug,tower_http=debug");
    async {
        let h = Harness::new(false).await;
        let (_, _, token, _) = onboard_portal_user(&h, "log-user").await;
        capture.take();
        let response = h
            .client
            .get(format!(
                "{}/pypi/simple/demo/?signature=client-query-secret",
                h.base
            ))
            .header("accept", "application/vnd.pypi.simple.v1+json")
            .header("x-client-secret", "header-secret")
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        let metadata: Value = response.json().await.unwrap();
        let logs = capture.take();
        assert!(!format!("{logs:?}").contains(&token));
        assert!(logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch started",
            &[("registry", "pypi"), ("resource_kind", "metadata")]
        ));
        assert!(logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch completed",
            &[("status", "Some(200)")]
        ));
        assert!(
            logged(
                &logs,
                tracing::Level::DEBUG,
                "Registry request completed",
                &[
                    ("method", "GET"),
                    ("path", "/pypi/simple/demo/"),
                    ("status", "200")
                ]
            ),
            "Captured events: {logs:#?}"
        );
        h.pypi().await;
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::DEBUG,
            "Cache outcome",
            &[("cache", "hit"), ("resource_kind", "metadata")]
        ));
        assert!(!logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch started",
            &[]
        ));
        let artifact = metadata["files"][0]["url"].as_str().unwrap();
        assert_eq!(h.client.head(artifact).send().await.unwrap().status(), 200);
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch completed",
            &[("method", "HEAD"), ("bytes", "0")]
        ));
        assert!(logged(
            &logs,
            tracing::Level::DEBUG,
            "Registry request completed",
            &[("method", "HEAD")]
        ));
        assert_eq!(
            h.client
                .get(artifact)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap(),
            "fixture archive"
        );
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch completed",
            &[("resource_kind", "artifact"), ("bytes", "15")]
        ));
        assert_eq!(h.client.get(artifact).send().await.unwrap().status(), 200);
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::DEBUG,
            "Cache outcome",
            &[("resource_kind", "artifact"), ("cache", "hit")]
        ));
        assert!(!logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch started",
            &[]
        ));
        h.clock.0.fetch_add(301, Ordering::SeqCst);
        h.pypi().await;
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch completed",
            &[("status", "Some(304)"), ("bytes", "0")]
        ));
        assert!(logged(
            &logs,
            tracing::Level::DEBUG,
            "Cache outcome",
            &[("cache", "revalidated")]
        ));
        h.clock.0.fetch_add(301, Ordering::SeqCst);
        h.upstream.malformed.store(true, Ordering::SeqCst);
        h.pypi().await;
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::WARN,
            "Upstream fetch failed",
            &[("error", "Invalid upstream JSON metadata")]
        ));
        assert!(logs.iter().any(|log| {
            log.level == tracing::Level::WARN
                && log.fields.get("message").is_some_and(|message| {
                    message == "Serving stale metadata after upstream failure"
                })
        }));
        h.upstream.malformed.store(false, Ordering::SeqCst);
        h.upstream.offline.store(true, Ordering::SeqCst);
        h.pypi().await;
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::WARN,
            "Upstream fetch failed",
            &[("status", "Some(503)")]
        ));
        assert!(logged(
            &logs,
            tracing::Level::WARN,
            "Serving stale metadata after upstream failure",
            &[("cache", "stale")]
        ));
        h.upstream.offline.store(false, Ordering::SeqCst);
        h.upstream.bad_hash.store(true, Ordering::SeqCst);
        assert_eq!(
            h.client
                .get(metadata["files"][1]["url"].as_str().unwrap())
                .send()
                .await
                .unwrap()
                .status(),
            502
        );
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::WARN,
            "Upstream fetch failed",
            &[
                ("resource_kind", "artifact"),
                ("bytes", "15"),
                ("error", "Artifact checksum mismatch")
            ]
        ));
        assert!(!logged(
            &logs,
            tracing::Level::INFO,
            "Upstream fetch completed",
            &[("resource_kind", "artifact")]
        ));
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let broken_url = format!(
            "http://{}/truncated.whl?signature=upstream-query-secret",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\nConnection: close\r\n\r\nshort",
                )
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
        });
        let item = cache::register(
            &h.app,
            &h.app.snapshot().await,
            ArtifactInput {
                registry: "pypi",
                package: "truncated",
                version: "1",
                filename: "truncated.whl",
                url: &broken_url,
                checksums: BTreeMap::new(),
                sidecar: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            h.client
                .get(format!("{}/pypi/files/{}/truncated.whl", h.base, item.id))
                .send()
                .await
                .unwrap()
                .status(),
            502
        );
        server.await.unwrap();
        let logs = capture.take();
        assert!(logged(
            &logs,
            tracing::Level::WARN,
            "Upstream fetch failed",
            &[
                ("status", "Some(200)"),
                ("path", "/truncated.whl"),
                ("error", "Interrupted artifact download")
            ]
        ));
        assert!(!tokio::fs::try_exists(item.path(&h.app)).await.unwrap());
        let runtime = h.app.snapshot().await;
        for kind in ["npm", "cargo"] {
            let path = if kind == "npm" { "demo" } else { "de/mo/demo" };
            cache::metadata(
                &h.app,
                &runtime,
                kind,
                &format!("{}{path}", runtime.settings.registries[kind].upstream_url),
                "application/json",
            )
            .await
            .unwrap();
            assert!(logged(
                &capture.take(),
                tracing::Level::INFO,
                "Upstream fetch completed",
                &[("registry", kind)]
            ));
        }
    }
    .with_subscriber(subscriber)
    .await;
}

#[tokio::test]
async fn info_logging_keeps_upstream_events_and_filters_request_and_cache_details() {
    let capture = LogCapture::default();
    let subscriber = log_subscriber(capture.clone(), "speculum=info,tower_http=info");
    async {
        let h = Harness::new(false).await;
        capture.take();
        h.pypi().await;
        let logs = capture.take();
        assert!(
            logged(&logs, tracing::Level::INFO, "Upstream fetch completed", &[]),
            "Captured events: {logs:#?}"
        );
        assert!(logs.iter().all(|log| log.level != tracing::Level::DEBUG));
        h.pypi().await;
        assert!(capture.take().is_empty());
    }
    .with_subscriber(subscriber)
    .await;
}
