use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    response::Response,
};
use bytes::Bytes;
use reqwest::{Client, Method};
use sha2::{Digest, Sha256, Sha512};
use speculum::{App, app::Clock, config::DISTROS};
use sqlx::Row;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;
use tokio::task::JoinHandle;

struct TestClock(AtomicI64);
impl Clock for TestClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
#[derive(Default)]
struct Upstream {
    bodies: Mutex<HashMap<String, Bytes>>,
    calls: AtomicUsize,
    heads: AtomicUsize,
    conditional: AtomicUsize,
    status: AtomicU16,
    interrupted: AtomicBool,
    seen_auth: AtomicBool,
}
async fn upstream(State(state): State<Arc<Upstream>>, request: Request) -> Response {
    if request.headers().contains_key("authorization") {
        state.seen_auth.store(true, Ordering::SeqCst);
    }
    if request.method() == Method::HEAD {
        state.heads.fetch_add(1, Ordering::SeqCst);
    } else {
        state.calls.fetch_add(1, Ordering::SeqCst);
    }
    let status = state.status.load(Ordering::SeqCst);
    if status != 0 {
        return Response::builder()
            .status(status)
            .body(Body::empty())
            .unwrap();
    }
    if request.uri().path().ends_with("redirect") {
        return Response::builder()
            .status(302)
            .header("location", "http://localhost:9/outside")
            .body(Body::empty())
            .unwrap();
    }
    let body = state
        .bodies
        .lock()
        .unwrap()
        .get(request.uri().path())
        .cloned()
        .unwrap_or_else(|| Bytes::from_static(b"signed\0repository\n"));
    let etag = format!("\"{:x}\"", Sha256::digest(&body));
    if request
        .headers()
        .get("if-none-match")
        .is_some_and(|value| value == etag.as_str())
    {
        state.conditional.fetch_add(1, Ordering::SeqCst);
        return Response::builder().status(304).body(Body::empty()).unwrap();
    }
    if state.interrupted.load(Ordering::SeqCst) {
        let stream = futures_util::stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"partial")),
            Err(std::io::Error::other("fixture interrupted")),
        ]);
        return Response::builder()
            .header("content-length", "1000")
            .body(Body::from_stream(stream))
            .unwrap();
    }
    Response::builder()
        .header("content-type", "application/octet-stream")
        .header("etag", etag)
        .header("last-modified", "Tue, 14 Nov 2023 22:13:20 GMT")
        .header("content-length", body.len())
        .body(if request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from(body)
        })
        .unwrap()
}
struct Harness {
    app: Arc<App>,
    clock: Arc<TestClock>,
    upstream: Arc<Upstream>,
    base: String,
    client: Client,
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
    async fn new(auth: bool) -> Self {
        let fixture = Arc::new(Upstream::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().fallback(upstream).with_state(fixture.clone());
        let origin_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(1700000000)));
        let app = App::open_with_clock(dir.path(), clock.clone())
            .await
            .unwrap();
        let mut settings = app.snapshot().await.settings;
        for kind in DISTROS {
            let config = settings.registries.get_mut(kind).unwrap();
            config.upstream_url = format!("{origin}/up/{kind}/");
            config.auth_required = auth;
        }
        app.save_settings(settings).await.unwrap();
        sqlx::query("INSERT INTO users(id,username,password_hash,must_change_password,enabled,created_at) VALUES('user','alice','unused',0,1,0)").execute(&app.db).await.unwrap();
        sqlx::query("INSERT INTO tokens(id,user_id,name,prefix,hash,created_at) VALUES('token','user','fixture','spm_fixture',?,0)")
            .bind(speculum::auth::digest(Self::token())).execute(&app.db).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = speculum::router(app.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            app,
            clock,
            upstream: fixture,
            base,
            client: Client::builder().no_proxy().build().unwrap(),
            _dir: dir,
            tasks: vec![origin_task, task],
        }
    }
    fn token() -> &'static str {
        "spm_fixture_token_that_is_long_enough_for_tests"
    }
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .basic_auth("alice", Some(Self::token()))
    }
    async fn get(&self, path: &str) -> reqwest::Response {
        self.request(Method::GET, path).send().await.unwrap()
    }
    fn body(&self, path: &str, bytes: impl Into<Bytes>) {
        self.upstream
            .bodies
            .lock()
            .unwrap()
            .insert(format!("/up{path}"), bytes.into());
    }
    fn advance(&self) {
        self.clock.0.fetch_add(301, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn distro_authentication_covers_indexes_packages_and_cache_hits() {
    let h = Harness::new(true).await;
    for kind in DISTROS {
        let path = if kind == "alpine" {
            format!("/{kind}/v3.23/main/x86_64/APKINDEX.tar.gz")
        } else {
            format!("/{kind}/dists/stable/InRelease")
        };
        let response = h
            .client
            .get(format!("{}{path}", h.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert!(
            response.headers()["www-authenticate"]
                .to_str()
                .unwrap()
                .starts_with("Basic ")
        );
        assert_eq!(
            h.get(&path).await.bytes().await.unwrap(),
            "signed\0repository\n"
        );
        assert_eq!(h.get(&path).await.status(), 200);
        assert_eq!(
            h.client
                .get(format!("{}{path}", h.base))
                .basic_auth("bob", Some(Harness::token()))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            h.client
                .get(format!("{}{path}", h.base))
                .bearer_auth(Harness::token())
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert!(!h.upstream.seen_auth.load(Ordering::SeqCst));
    let used: i64 =
        sqlx::query_scalar("SELECT last_used_at FROM token_usage WHERE token_id='token'")
            .fetch_one(&h.app.db)
            .await
            .unwrap();
    assert_eq!(used, 1700000000);
    sqlx::query("UPDATE users SET enabled=0 WHERE id='user'")
        .execute(&h.app.db)
        .await
        .unwrap();
    assert_eq!(h.get("/debian/dists/stable/InRelease").await.status(), 401);
    sqlx::query("UPDATE users SET enabled=1 WHERE id='user'")
        .execute(&h.app.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM tokens")
        .execute(&h.app.db)
        .await
        .unwrap();
    assert_eq!(h.get("/debian/dists/stable/InRelease").await.status(), 401);
    let mut settings = h.app.snapshot().await.settings;
    settings.registries.get_mut("debian").unwrap().auth_required = false;
    settings.registries.get_mut("alpine").unwrap().enabled = false;
    h.app.save_settings(settings).await.unwrap();
    assert_eq!(h.get("/debian/dists/stable/InRelease").await.status(), 200);
    assert_eq!(h.get("/alpine/v3.23/main/x86_64/a.apk").await.status(), 404);
}

#[tokio::test]
async fn indexes_revalidate_and_preserve_signed_bytes_and_client_validators() {
    let h = Harness::new(false).await;
    let path = "/debian/dists/stable/InRelease";
    h.body(path, Bytes::from_static(b"first\0signed\r\n"));
    let first = h.get(path).await;
    let etag = first.headers()["etag"].clone();
    assert_eq!(first.bytes().await.unwrap(), b"first\0signed\r\n"[..]);
    assert_eq!(
        h.request(Method::GET, path)
            .header("if-none-match", &etag)
            .send()
            .await
            .unwrap()
            .status(),
        304
    );
    h.advance();
    assert_eq!(h.get(path).await.status(), 200);
    assert_eq!(h.upstream.conditional.load(Ordering::SeqCst), 1);
    h.body(path, "second signed content");
    let response = h
        .request(Method::GET, path)
        .header("cache-control", "no-cache")
        .send()
        .await
        .unwrap();
    assert_ne!(response.headers()["etag"], etag);
    assert_eq!(response.text().await.unwrap(), "second signed content");
    let response = h
        .request(Method::GET, path)
        .header("range", "bytes=0-5")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.text().await.unwrap(), "second");
    assert_eq!(
        h.request(Method::GET, path)
            .header("range", "bytes=0-5")
            .header("if-range", &etag)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        h.request(Method::GET, path)
            .header("range", "bytes=500-")
            .send()
            .await
            .unwrap()
            .status(),
        416
    );
}

#[tokio::test]
async fn immutable_files_are_isolated_and_by_hash_is_verified() {
    let h = Harness::new(false).await;
    for path in [
        "/debian/pool/main/f/foo/foo_1_amd64.deb",
        "/alpine/v3.23/main/x86_64/foo-1-r0.apk",
        "/alpine/v3.23/main/aarch64/foo-1-r0.apk",
    ] {
        h.body(path, path.to_owned());
        assert_eq!(h.get(path).await.text().await.unwrap(), path);
    }
    h.advance();
    h.upstream.status.store(503, Ordering::SeqCst);
    assert_eq!(
        h.get("/alpine/v3.23/main/x86_64/foo-1-r0.apk")
            .await
            .status(),
        200
    );
    h.upstream.status.store(0, Ordering::SeqCst);
    for (algorithm, digest) in [
        ("SHA256", format!("{:x}", Sha256::digest(b"index"))),
        ("SHA512", format!("{:x}", Sha512::digest(b"index"))),
    ] {
        let path = format!("/debian/dists/stable/main/binary-amd64/by-hash/{algorithm}/{digest}");
        h.body(&path, "index");
        assert_eq!(h.get(&path).await.text().await.unwrap(), "index");
    }
    h.upstream.status.store(503, Ordering::SeqCst);
    for path in [
        "/debian/pool/main/f/foo/foo_1_amd64.deb".to_owned(),
        "/alpine/v3.23/main/x86_64/foo-1-r0.apk".to_owned(),
        format!(
            "/debian/dists/stable/main/binary-amd64/by-hash/SHA512/{:x}",
            Sha512::digest(b"index")
        ),
    ] {
        for method in [Method::GET, Method::HEAD] {
            assert_eq!(
                h.request(method, &path)
                    .header("cache-control", "no-cache")
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        }
    }
    let mut settings = h.app.snapshot().await.settings;
    settings
        .registries
        .get_mut("alpine")
        .unwrap()
        .cache_lifetime_seconds = Some(1);
    h.app.save_settings(settings).await.unwrap();
    assert_eq!(
        h.get("/alpine/v3.23/main/x86_64/foo-1-r0.apk")
            .await
            .status(),
        502
    );
    h.upstream.status.store(0, Ordering::SeqCst);
    let bad = format!(
        "/debian/dists/stable/main/binary-amd64/by-hash/SHA256/{}",
        "0".repeat(64)
    );
    assert_eq!(h.get(&bad).await.status(), 502);
    assert_eq!(
        std::fs::read_dir(h._dir.path().join("tmp"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn stale_fallback_is_limited_to_network_and_server_errors() {
    let h = Harness::new(false).await;
    let path = "/alpine/v3.23/main/x86_64/APKINDEX.tar.gz";
    assert_eq!(h.get(path).await.status(), 200);
    h.advance();
    h.upstream.status.store(503, Ordering::SeqCst);
    assert_eq!(
        h.get(path).await.text().await.unwrap(),
        "signed\0repository\n"
    );
    assert_eq!(h.get("/debian/dists/new/InRelease").await.status(), 502);
    h.upstream.status.store(404, Ordering::SeqCst);
    assert_eq!(h.get(path).await.status(), 404);
    h.upstream.status.store(304, Ordering::SeqCst);
    assert_eq!(h.get("/debian/dists/new/InRelease").await.status(), 502);
    h.upstream.status.store(0, Ordering::SeqCst);
    h.body(path, "new body");
    h.upstream.interrupted.store(true, Ordering::SeqCst);
    assert_eq!(
        h.get(path).await.text().await.unwrap(),
        "signed\0repository\n"
    );
    h.upstream.interrupted.store(false, Ordering::SeqCst);
    h.tasks[0].abort();
    tokio::task::yield_now().await;
    assert_eq!(
        h.get(path).await.text().await.unwrap(),
        "signed\0repository\n"
    );
}

#[tokio::test]
async fn cold_head_does_not_download_and_concurrent_gets_share_a_body() {
    let h = Harness::new(false).await;
    let path = "/debian/pool/main/f/foo/foo_1_amd64.deb";
    assert_eq!(
        h.request(Method::HEAD, path).send().await.unwrap().status(),
        200
    );
    assert_eq!(h.upstream.heads.load(Ordering::SeqCst), 1);
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 0);
    let responses = futures_util::future::join_all((0..12).map(|_| h.get(path))).await;
    for response in responses {
        assert_eq!(response.bytes().await.unwrap(), "signed\0repository\n");
    }
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.request(Method::HEAD, path).send().await.unwrap().status(),
        200
    );
    assert_eq!(h.upstream.heads.load(Ordering::SeqCst), 1);
    let row = sqlx::query("SELECT requests,upstream_downloads FROM packages WHERE name=?")
        .bind(path.trim_start_matches("/debian/"))
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("requests"), 12);
    assert_eq!(row.get::<i64, _>("upstream_downloads"), 1);
}

#[tokio::test]
async fn large_indexes_evict_cleanly_and_changed_origins_do_not_reuse_files() {
    let h = Harness::new(false).await;
    let path = "/debian/dists/stable/main/binary-amd64/Packages";
    let bytes = Bytes::from(vec![b'X'; 65 * 1024 * 1024]);
    h.body(path, bytes.clone());
    assert_eq!(h.get(path).await.bytes().await.unwrap(), bytes);
    let id: String = sqlx::query_scalar("SELECT artifact_id FROM distro_resources WHERE relative_path='dists/stable/main/binary-amd64/Packages'").fetch_one(&h.app.db).await.unwrap();
    speculum::cache::evict(&h.app, &id, false).await.unwrap();
    assert_eq!(
        std::fs::read_dir(h._dir.path().join("distro"))
            .unwrap()
            .count(),
        0
    );
    h.body(path, "new index");
    assert_eq!(h.get(path).await.text().await.unwrap(), "new index");
    let mut settings = h.app.snapshot().await.settings;
    settings
        .registries
        .get_mut("debian")
        .unwrap()
        .upstream_url
        .push_str("changed/");
    h.app.save_settings(settings).await.unwrap();
    assert_eq!(
        h.get(path).await.text().await.unwrap(),
        "signed\0repository\n"
    );
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn paths_and_redirects_cannot_escape_the_configured_upstream() {
    let h = Harness::new(false).await;
    for path in [
        "/debian/dists/%252e%252e/secret",
        "/debian/dists/a%2fb%2f..%2f..%2fsecret",
        "/debian/dists/%5csecret",
        "/debian/dists/a%00b",
        "/debian/dists/a%3fb",
        "/debian/dists/a%23b",
    ] {
        assert_eq!(h.get(path).await.status(), 400, "{path}");
    }
    assert_eq!(h.upstream.calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.get("/debian/dists/redirect").await.status(), 502);
    assert_eq!(h.get("/debian/not-a-repository/file").await.status(), 404);
}

#[tokio::test]
async fn index_refresh_waits_for_readers_and_does_not_replace_their_generation() {
    let h = Arc::new(Harness::new(false).await);
    let path = "/debian/dists/stable/main/binary-amd64/Packages";
    let original = Bytes::from(vec![b'A'; 16 * 1024 * 1024]);
    h.body(path, original.clone());
    let reader = h.get(path).await;
    h.body(path, "replacement index");
    let refresh_harness = h.clone();
    let refresh = tokio::spawn(async move {
        refresh_harness
            .request(Method::GET, path)
            .header("cache-control", "no-cache")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !refresh.is_finished(),
        "refresh must wait for the prior generation's reader"
    );
    assert_eq!(reader.bytes().await.unwrap(), original);
    assert_eq!(refresh.await.unwrap(), "replacement index");
    assert_eq!(
        std::fs::read_dir(h._dir.path().join("distro"))
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn distro_retention_cleanup_and_registry_clear_preserve_statistics() {
    let h = Harness::new(false).await;
    let path = "/debian/dists/stable/InRelease";
    assert_eq!(h.get(path).await.status(), 200);
    let mut settings = h.app.snapshot().await.settings;
    settings
        .registries
        .get_mut("debian")
        .unwrap()
        .cache_lifetime_seconds = Some(1);
    h.app.save_settings(settings).await.unwrap();
    h.advance();
    speculum::cache::cleanup(&h.app).await.unwrap();
    assert_eq!(
        std::fs::read_dir(h._dir.path().join("distro"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(h.get(path).await.status(), 200);
    sqlx::query("UPDATE admin SET must_change_password=0")
        .execute(&h.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO sessions(id,csrf,created_at) VALUES(?,'fixture-csrf',?)")
        .bind(speculum::auth::digest("fixture-session"))
        .bind(h.clock.now())
        .execute(&h.app.db)
        .await
        .unwrap();
    let result = h
        .client
        .delete(format!("{}/api/admin/cache/debian", h.base))
        .header("cookie", "speculum_session=fixture-session")
        .header("x-csrf-token", "fixture-csrf")
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 204);
    assert_eq!(
        std::fs::read_dir(h._dir.path().join("distro"))
            .unwrap()
            .count(),
        0
    );
    let requests: i64 = sqlx::query_scalar("SELECT requests FROM packages WHERE registry='debian'")
        .fetch_one(&h.app.db)
        .await
        .unwrap();
    assert_eq!(requests, 2);
    h.upstream.status.store(503, Ordering::SeqCst);
    assert_eq!(h.get(path).await.status(), 502);
}

#[tokio::test]
async fn disk_generations_survive_restart_and_missing_bodies_are_invalidated() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    let runtime = app.snapshot().await;
    let item = speculum::cache::register(
        &app,
        &runtime,
        speculum::cache::ArtifactInput {
            registry: "debian",
            package: "dists/stable/InRelease",
            version: "",
            filename: "InRelease",
            url: "https://deb.debian.org/debian/dists/stable/InRelease",
            checksums: Default::default(),
            sidecar: true,
        },
    )
    .await
    .unwrap();
    let generation = uuid::Uuid::new_v4().to_string();
    let body_path = dir.path().join("distro").join(&generation);
    std::fs::write(&body_path, b"cached signed index").unwrap();
    sqlx::query("UPDATE artifacts SET downloaded_at=1,size=19 WHERE id=?")
        .bind(&item.id)
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO distro_resources(artifact_id,relative_path,immutable,generation,validated_at,body_hash) VALUES(?,'dists/stable/InRelease',0,?,1,'hash')")
        .bind(&item.id).bind(&generation).execute(&app.db).await.unwrap();
    drop(runtime);
    drop(app);
    let app = App::open(dir.path()).await.unwrap();
    assert_eq!(std::fs::read(&body_path).unwrap(), b"cached signed index");
    assert!(
        speculum::cache::artifact(&app, &item.id)
            .await
            .unwrap()
            .downloaded_at
            .is_some()
    );
    drop(app);
    std::fs::remove_file(body_path).unwrap();
    let app = App::open(dir.path()).await.unwrap();
    assert!(
        speculum::cache::artifact(&app, &item.id)
            .await
            .unwrap()
            .downloaded_at
            .is_none()
    );
    let generation: Option<String> =
        sqlx::query_scalar("SELECT generation FROM distro_resources WHERE artifact_id=?")
            .bind(&item.id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(generation.is_none());
}

#[tokio::test]
async fn existing_settings_users_tokens_artifacts_and_generations_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(dir.path()).await.unwrap();
    let mut settings = app.snapshot().await.settings;
    for kind in DISTROS {
        settings.registries.remove(kind);
    }
    settings.registries.get_mut("npm").unwrap().enabled = false;
    let encrypted = app
        .secret
        .encrypt(&serde_json::to_vec(&settings).unwrap())
        .unwrap();
    sqlx::query("UPDATE settings SET value=?")
        .bind(encrypted)
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,username,password_hash,enabled,created_at) VALUES('legacy','legacy','hash',1,0)").execute(&app.db).await.unwrap();
    sqlx::query("INSERT INTO tokens(id,user_id,name,prefix,hash,created_at) VALUES('legacy','legacy','name','prefix','hash',0)").execute(&app.db).await.unwrap();
    let runtime = app.snapshot().await;
    let old = speculum::cache::register(
        &app,
        &runtime,
        speculum::cache::ArtifactInput {
            registry: "pypi",
            package: "legacy",
            version: "1",
            filename: "legacy.whl",
            url: "https://files.pythonhosted.org/legacy.whl",
            checksums: Default::default(),
            sidecar: false,
        },
    )
    .await
    .unwrap();
    std::fs::write(old.path(&app), b"legacy cached wheel").unwrap();
    sqlx::query("UPDATE artifacts SET downloaded_at=1,size=19 WHERE id=?")
        .bind(&old.id)
        .execute(&app.db)
        .await
        .unwrap();
    std::fs::write(dir.path().join("distro/orphan"), b"orphan").unwrap();
    drop(runtime);
    drop(app);
    let app = App::open(dir.path()).await.unwrap();
    let settings = app.snapshot().await.settings;
    assert_eq!(settings.registries.len(), 9);
    assert!(!settings.registries["npm"].enabled);
    app.save_settings(settings).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tokens WHERE id='legacy'")
            .fetch_one(&app.db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        std::fs::read(old.path(&app)).unwrap(),
        b"legacy cached wheel"
    );
    assert!(!dir.path().join("distro/orphan").exists());
}
