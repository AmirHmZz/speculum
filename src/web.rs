use crate::{App, admin, auth, registries, user};
use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, Request, State},
    http::{Method, StatusCode},
    middleware,
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use std::sync::Arc;
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};
mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

pub fn router(app: Arc<App>) -> Router {
    let admin_routes = Router::new()
        .route("/session", get(auth::me).delete(auth::logout))
        .route(
            "/settings",
            get(admin::settings).put(admin::update_settings),
        )
        .route("/credentials", axum::routing::put(admin::update_admin))
        .route("/users", get(admin::users).post(admin::create_user))
        .route(
            "/users/{id}",
            axum::routing::put(admin::update_user).delete(admin::delete_user),
        )
        .route("/users/{id}/tokens", post(admin::create_token))
        .route(
            "/users/{id}/reset-password",
            post(admin::reset_user_password),
        )
        .route("/tokens/{id}", delete(admin::delete_token))
        .route("/summary", get(admin::summary))
        .route("/packages", get(admin::packages))
        .route(
            "/packages/{id}",
            get(admin::package).delete(admin::delete_package),
        )
        .route("/artifacts/{id}", delete(admin::delete_artifact))
        .route("/cache/{kind}", delete(admin::clear_registry))
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            auth::admin_guard,
        ));
    let user_routes = Router::new()
        .route("/session", get(auth::user_me).delete(auth::user_logout))
        .route("/password", axum::routing::put(auth::user_password))
        .route("/tokens", get(user::tokens).post(user::create_token))
        .route("/tokens/{id}", delete(user::delete_token))
        .route("/summary", get(user::summary))
        .route("/packages", get(user::packages))
        .route("/config", get(user::config))
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            auth::user_guard,
        ));
    let mirrors = Router::new()
        .route("/pypi", get(mirror_root))
        .route("/pypi/", get(mirror_root))
        .route("/pypi/simple", get(mirror_root))
        .route("/pypi/simple/", get(registries::pypi::root))
        .route("/pypi/simple/{name}/", get(registries::pypi::project))
        .route("/pypi/simple/{name}", get(registries::pypi::project))
        .route("/pypi/files/{id}/{filename}", get(registries::pypi::file))
        .route("/npm/", get(registries::npm::root))
        .route("/npm", get(mirror_root))
        .route("/npm/files/{id}/{filename}", get(npm_file))
        .route(
            "/npm/-/npm/v1/security/advisories/bulk",
            post(registries::npm::audit),
        )
        .route(
            "/npm/-/npm/v1/security/audits/quick",
            post(registries::npm::audit),
        )
        .route("/npm/{*path}", get(registries::npm::metadata))
        .route("/cargo/index/config.json", get(registries::cargo::config))
        .route("/cargo", get(mirror_root))
        .route("/cargo/", get(mirror_root))
        .route("/cargo/index", get(mirror_root))
        .route("/cargo/index/", get(mirror_root))
        .route("/cargo/index/{*path}", get(registries::cargo::index))
        .route(
            "/cargo/crates/{name}/{version}/download",
            get(registries::cargo::download),
        )
        .route_layer(middleware::from_fn_with_state(
            app.clone(),
            auth::mirror_guard,
        ))
        .route_layer(middleware::from_fn(registry_request_log));
    Router::new()
        .nest("/api/admin", admin_routes)
        .nest("/api/user", user_routes)
        .route("/api/admin/login", post(auth::login))
        .route("/api/user/login", post(auth::user_login))
        .route("/api/login", post(auth::dashboard_login))
        .route("/healthz", get(health))
        .merge(mirrors)
        .fallback(static_assets)
        .layer(RequestBodyLimitLayer::new(4 * 1024 * 1024))
        .layer(middleware::from_fn(security_headers))
        .layer(TraceLayer::new_for_http().make_span_with(|request: &Request| {
            tracing::debug_span!("http_request", method=%request.method(), path=request.uri().path())
        }))
        .with_state(app)
}
async fn mirror_root(OriginalUri(uri): OriginalUri) -> Redirect {
    let path = match uri.path() {
        "/npm" => "/npm/",
        "/pypi" | "/pypi/" | "/pypi/simple" => "/pypi/simple/",
        _ => "/cargo/index/config.json",
    };
    let target = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    Redirect::permanent(&target)
}
async fn registry_request_log(request: Request, next: middleware::Next) -> Response {
    let started = std::time::Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let registry = path.split('/').nth(1).unwrap_or("");
    let response = next.run(request).await;
    tracing::debug!(%method, registry, path, status=response.status().as_u16(), elapsed_ms=started.elapsed().as_millis() as u64, "Registry request completed");
    response
}
async fn npm_file(
    State(app): State<Arc<App>>,
    axum::extract::Path((id, filename)): axum::extract::Path<(String, String)>,
    headers: axum::http::HeaderMap,
    method: Method,
    identity: Option<axum::Extension<auth::MirrorIdentity>>,
) -> crate::error::Result<Response> {
    crate::cache::serve(
        app,
        "npm",
        &id,
        Some(&filename),
        headers,
        method,
        identity.map(|identity| identity.0.0),
    )
    .await
}
async fn health(State(app): State<Arc<App>>) -> StatusCode {
    if sqlx::query("SELECT 1").execute(&app.db).await.is_ok() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
async fn static_assets(request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = if path.is_empty()
        || path == "admin"
        || path.starts_with("admin/")
        || path == "account"
        || path.starts_with("account/")
        || path == "login"
        || path == "login/"
    {
        "index.html"
    } else {
        path
    };
    if let Some(bytes) = assets::asset(path) {
        let mut response = Response::new(if request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from(bytes)
        });
        response.headers_mut().insert(
            "content-type",
            mime_guess::from_path(path)
                .first_or_octet_stream()
                .as_ref()
                .parse()
                .unwrap(),
        );
        response.headers_mut().insert(
            "cache-control",
            if path == "index.html" {
                "no-cache"
            } else {
                "public, max-age=31536000, immutable"
            }
            .parse()
            .unwrap(),
        );
        response
            .headers_mut()
            .insert("content-length", bytes.len().into());
        response
    } else if path == "index.html" {
        (StatusCode::SERVICE_UNAVAILABLE,"Dashboard assets missing. Run bun install and bun run build in dashboard/, then rebuild Rust.").into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
async fn security_headers(request: Request, next: middleware::Next) -> Response {
    let api = request.uri().path().starts_with("/api/");
    let mut response = next.run(request).await;
    if api {
        response
            .headers_mut()
            .insert("cache-control", "no-store".parse().unwrap());
    }
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "same-origin".parse().unwrap());
    response
        .headers_mut()
        .insert("x-frame-options", "DENY".parse().unwrap());
    response.headers_mut().insert("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'".parse().unwrap());
    response
}
