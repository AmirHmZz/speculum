use crate::{
    App,
    error::{Error, Result},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{net::SocketAddr, sync::Arc};

pub fn validate_username(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(Error::bad(
            "Username must contain 1–64 letters, numbers, dots, underscores, or hyphens",
        ));
    }
    Ok(())
}
pub async fn hash_password(password: String) -> Result<String> {
    if !(10..=1024).contains(&password.len()) {
        return Err(Error::bad("Password must contain 10–1024 bytes"));
    }
    tokio::task::spawn_blocking(move || {
        let mut bytes = [0; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| Error::bad("Random generator failed"))?;
        let salt =
            SaltString::encode_b64(&bytes).map_err(|_| Error::bad("Invalid password salt"))?;
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|_| Error::bad("Unable to hash password"))
    })
    .await
    .map_err(|_| Error::bad("Unable to hash password"))?
}
pub async fn verify_password(password: String, hash: String) -> bool {
    if password.len() > 1024 {
        return false;
    }
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false)
}
pub fn random_secret() -> Result<String> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::bad("Random generator failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[derive(Clone, Copy)]
pub enum Role {
    Admin,
    User,
}
impl Role {
    fn table(self) -> &'static str {
        match self {
            Self::Admin => "sessions",
            Self::User => "user_sessions",
        }
    }
    fn cookie(self) -> &'static str {
        match self {
            Self::Admin => "speculum_session",
            Self::User => "speculum_user_session",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Admin => "/api/admin",
            Self::User => "/api/user",
        }
    }
}
#[derive(Clone)]
pub struct DashboardSession {
    pub id: String,
    pub user_id: Option<String>,
    pub username: String,
    pub csrf: String,
    pub must_change_password: bool,
}
#[derive(Clone)]
pub struct MirrorIdentity(pub String);

fn cookie(headers: &HeaderMap, role: Role) -> Option<&str> {
    let prefix = format!("{}=", role.cookie());
    headers
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| part.trim().strip_prefix(&prefix))
}
pub async fn session(app: &App, headers: &HeaderMap, mutation: bool) -> Result<(String, String)> {
    let session = dashboard_session(app, headers, mutation, Role::Admin).await?;
    Ok((session.id, session.csrf))
}
pub async fn dashboard_session(
    app: &App,
    headers: &HeaderMap,
    mutation: bool,
    role: Role,
) -> Result<DashboardSession> {
    let value = cookie(headers, role)
        .filter(|value| value.len() <= 128)
        .ok_or_else(Error::unauthorized)?;
    let sql = match role {
        Role::Admin => {
            "SELECT s.id,s.csrf,s.created_at,a.username,a.must_change_password,NULL AS user_id FROM sessions s JOIN admin a ON a.id=1 WHERE s.id=?"
        }
        Role::User => {
            "SELECT s.id,s.csrf,s.created_at,u.username,u.must_change_password,u.id AS user_id FROM user_sessions s JOIN users u ON u.id=s.user_id WHERE s.id=? AND u.enabled=1"
        }
    };
    let row = sqlx::query(sql)
        .bind(digest(value))
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(Error::unauthorized)?;
    let settings = app.snapshot().await.settings;
    let lifetime = match role {
        Role::Admin => settings.admin_session_lifetime_seconds,
        Role::User => settings.user_session_lifetime_seconds,
    };
    if app.now().saturating_sub(row.get::<i64, _>("created_at")) >= lifetime as i64 {
        return Err(Error::unauthorized());
    }
    let csrf: String = row.get("csrf");
    if mutation && headers.get("x-csrf-token").and_then(|h| h.to_str().ok()) != Some(csrf.as_str())
    {
        return Err(Error(StatusCode::FORBIDDEN, "Invalid CSRF token".into()));
    }
    Ok(DashboardSession {
        id: row.get("id"),
        user_id: row.get("user_id"),
        username: row.get("username"),
        csrf,
        must_change_password: row.get("must_change_password"),
    })
}
fn session_json(session: &DashboardSession) -> Value {
    let role = if session.user_id.is_some() {
        "user"
    } else {
        "admin"
    };
    json!({"username":session.username,"csrf_token":session.csrf,"must_change_password":session.must_change_password,"role":role})
}
async fn cookie_header(app: &App, role: Role, value: &str) -> String {
    let secure = app
        .snapshot()
        .await
        .settings
        .public_base_url
        .starts_with("https://");
    format!(
        "{}={value}; Path={}; HttpOnly; SameSite=Strict{}",
        role.cookie(),
        role.path(),
        if secure { "; Secure" } else { "" }
    )
}
async fn session_response(
    app: &App,
    role: Role,
    value: &str,
    session: &DashboardSession,
) -> Response {
    (
        [
            ("set-cookie", cookie_header(app, role, value).await),
            ("cache-control", "no-store".to_owned()),
        ],
        Json(session_json(session)),
    )
        .into_response()
}
async fn guard(app: Arc<App>, mut request: Request, next: Next, role: Role) -> Result<Response> {
    let session = dashboard_session(
        &app,
        request.headers(),
        !matches!(*request.method(), Method::GET | Method::HEAD),
        role,
    )
    .await?;
    let path = request
        .uri()
        .path()
        .strip_prefix(role.path())
        .unwrap_or(request.uri().path());
    let password_path = match role {
        Role::Admin => "credentials",
        Role::User => "password",
    };
    if session.must_change_password && path != "/session" && path != format!("/{password_path}") {
        return Err(Error(
            StatusCode::FORBIDDEN,
            "Password change required".into(),
        ));
    }
    let value = cookie(request.headers(), role).unwrap().to_owned();
    request.extensions_mut().insert(session);
    let mut response = next.run(request).await;
    if !response.headers().contains_key("set-cookie") {
        response.headers_mut().insert(
            "set-cookie",
            cookie_header(&app, role, &value).await.parse().unwrap(),
        );
    }
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    Ok(response)
}
pub async fn admin_guard(
    State(app): State<Arc<App>>,
    request: Request,
    next: Next,
) -> Result<Response> {
    guard(app, request, next, Role::Admin).await
}
pub async fn user_guard(
    State(app): State<Arc<App>>,
    request: Request,
    next: Next,
) -> Result<Response> {
    guard(app, request, next, Role::User).await
}

pub async fn mirror_guard(
    State(app): State<Arc<App>>,
    mut request: Request,
    next: Next,
) -> Response {
    let kind = request.uri().path().split('/').nth(1).unwrap_or("");
    let snapshot = app.snapshot().await;
    let Some(config) = snapshot.settings.registries.get(kind) else {
        return Error::not_found().into_response();
    };
    if !config.enabled {
        return Error::not_found().into_response();
    }
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let identity = if let Some(encoded) = authorization.strip_prefix("Basic ") {
        if kind == "pypi" && encoded.len() <= 1024 {
            if let Ok(bytes) = STANDARD.decode(encoded) {
                if let Ok(credentials) = String::from_utf8(bytes) {
                    if let Some((username, token)) = credentials.split_once(':') {
                        token_user(&app, token, Some(username)).await
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        token_user(
            &app,
            authorization
                .strip_prefix("Bearer ")
                .unwrap_or(authorization),
            None,
        )
        .await
    };
    if identity.is_some()
        || !config.auth_required
        || kind == "cargo" && request.uri().path() == "/cargo/index/config.json"
    {
        if let Some(id) = identity {
            request.extensions_mut().insert(MirrorIdentity(id));
        }
        next.run(request).await
    } else {
        let mut response = Error::unauthorized().into_response();
        response.headers_mut().insert(
            "www-authenticate",
            if kind == "cargo" {
                format!(
                    "Cargo login_url=\"{}/account\"",
                    snapshot.settings.public_base_url.trim_end_matches('/')
                )
                .parse()
                .unwrap()
            } else if kind == "pypi" {
                "Basic realm=\"Speculum\"".parse().unwrap()
            } else {
                "Bearer realm=\"Speculum\"".parse().unwrap()
            },
        );
        response
    }
}
async fn token_user(app: &App, token: &str, username: Option<&str>) -> Option<String> {
    if token.len() < 32 || token.len() > 256 {
        return None;
    }
    let row=sqlx::query("SELECT users.id,users.username,tokens.id AS token_id FROM tokens JOIN users ON users.id=tokens.user_id WHERE tokens.hash=? AND users.enabled=1").bind(digest(token)).fetch_optional(&app.db).await.ok()??;
    if username.is_none_or(|name| row.get::<String, _>("username") == name) {
        if let Err(error) = sqlx::query(
            "INSERT INTO token_usage(token_id,last_used_at) VALUES(?,?) \
             ON CONFLICT(token_id) DO UPDATE SET last_used_at=excluded.last_used_at \
             WHERE token_usage.last_used_at<excluded.last_used_at",
        )
        .bind(row.get::<String, _>("token_id"))
        .bind(app.now())
        .execute(&app.db)
        .await
        {
            tracing::error!(%error, "Unable to record token usage");
        }
        Some(row.get("id"))
    } else {
        None
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Login {
    pub username: String,
    pub password: String,
}
fn throttle(app: &App, peer: SocketAddr, role: Option<Role>) -> Result<String> {
    let now = app.now();
    let key = format!("{}:{}", role.map_or("/api/login", Role::path), peer.ip());
    let mut attempts = app.login_attempts.lock().unwrap();
    attempts.retain(|_, (_, start)| now.saturating_sub(*start) < 300);
    let entry = attempts.entry(key.clone()).or_insert((0, now));
    if entry.0 >= 10 {
        return Err(Error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many login attempts; retry after five minutes".into(),
        ));
    }
    entry.0 += 1;
    Ok(key)
}
async fn sign_in(
    app: Arc<App>,
    peer: SocketAddr,
    input: Login,
    role: Option<Role>,
) -> Result<Response> {
    let key = throttle(&app, peer, role)?;
    let rows = match role {
        Some(Role::Admin) => sqlx::query("SELECT NULL AS user_id,username,password_hash,must_change_password FROM admin WHERE id=1 AND username=?").bind(&input.username).fetch_all(&app.db).await?,
        Some(Role::User) => sqlx::query("SELECT id AS user_id,username,password_hash,must_change_password FROM users WHERE username=? AND enabled=1").bind(&input.username).fetch_all(&app.db).await?,
        None => sqlx::query("SELECT NULL AS user_id,username,password_hash,must_change_password FROM admin WHERE id=1 AND username=? UNION ALL SELECT id AS user_id,username,password_hash,must_change_password FROM users WHERE username=? AND enabled=1").bind(&input.username).bind(&input.username).fetch_all(&app.db).await?,
    };
    let mut matched = None;
    for row in rows {
        if verify_password(input.password.clone(), row.get("password_hash")).await {
            matched = Some(row);
            break;
        }
    }
    let row = matched.ok_or_else(Error::unauthorized)?;
    let role = if row.get::<Option<String>, _>("user_id").is_some() {
        Role::User
    } else {
        Role::Admin
    };
    let hash: String = row.get("password_hash");
    let value = random_secret()?;
    let csrf = random_secret()?;
    let mut session = DashboardSession {
        id: digest(&value),
        user_id: row.get("user_id"),
        username: row.get("username"),
        csrf,
        must_change_password: row.get("must_change_password"),
    };
    let mut tx = app.db.begin().await?;
    // Recheck the password while inserting: a simultaneous reset must not mint a valid session.
    let result=match role {
        Role::Admin=>sqlx::query("INSERT INTO sessions(id,csrf,created_at) SELECT ?,?,? FROM admin WHERE id=1 AND password_hash=?").bind(&session.id).bind(&session.csrf).bind(app.now()).bind(&hash).execute(&mut *tx).await?,
        Role::User=>sqlx::query("INSERT INTO user_sessions(id,user_id,csrf,created_at) SELECT ?,id,?,? FROM users WHERE id=? AND enabled=1 AND password_hash=?").bind(&session.id).bind(&session.csrf).bind(app.now()).bind(&session.user_id).bind(&hash).execute(&mut *tx).await?,
    };
    if result.rows_affected() != 1 {
        return Err(Error::unauthorized());
    }
    // The session endpoint always reads live account state; retain the same state here.
    let flag: bool = match role {
        Role::Admin => {
            sqlx::query_scalar("SELECT must_change_password FROM admin WHERE id=1")
                .fetch_one(&mut *tx)
                .await?
        }
        Role::User => {
            sqlx::query_scalar("SELECT must_change_password FROM users WHERE id=?")
                .bind(&session.user_id)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    session.must_change_password = flag;
    tx.commit().await?;
    app.login_attempts.lock().unwrap().remove(&key);
    Ok(session_response(&app, role, &value, &session).await)
}
pub async fn login(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Login>,
) -> Result<Response> {
    sign_in(app, peer, input, Some(Role::Admin)).await
}
pub async fn user_login(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Login>,
) -> Result<Response> {
    sign_in(app, peer, input, Some(Role::User)).await
}
pub async fn dashboard_login(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(input): Json<Login>,
) -> Result<Response> {
    sign_in(app, peer, input, None).await
}
pub async fn me(Extension(session): Extension<DashboardSession>) -> Json<Value> {
    Json(session_json(&session))
}
pub async fn user_me(Extension(session): Extension<DashboardSession>) -> Json<Value> {
    Json(session_json(&session))
}
async fn sign_out(app: Arc<App>, session: DashboardSession, role: Role) -> Result<Response> {
    sqlx::query(&format!("DELETE FROM {} WHERE id=?", role.table()))
        .bind(session.id)
        .execute(&app.db)
        .await?;
    Ok((
        [(
            "set-cookie",
            format!(
                "{}=; Path={}; Max-Age=0; HttpOnly; SameSite=Strict",
                role.cookie(),
                role.path()
            ),
        )],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}
pub async fn logout(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
) -> Result<Response> {
    sign_out(app, session, Role::Admin).await
}
pub async fn user_logout(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
) -> Result<Response> {
    sign_out(app, session, Role::User).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordInput {
    pub current_password: String,
    pub password: String,
}
pub async fn change_password(
    app: Arc<App>,
    session: DashboardSession,
    input: PasswordInput,
    role: Role,
) -> Result<Response> {
    if input.current_password == input.password {
        return Err(Error::bad("Choose a different password"));
    }
    let previous: String = match role {
        Role::Admin => {
            sqlx::query_scalar("SELECT password_hash FROM admin WHERE id=1")
                .fetch_one(&app.db)
                .await?
        }
        Role::User => {
            sqlx::query_scalar("SELECT password_hash FROM users WHERE id=? AND enabled=1")
                .bind(&session.user_id)
                .fetch_optional(&app.db)
                .await?
                .ok_or_else(Error::unauthorized)?
        }
    };
    if !verify_password(input.current_password, previous.clone()).await {
        return Err(Error::bad("Current password is incorrect"));
    }
    let hash = hash_password(input.password).await?;
    let value = random_secret()?;
    let csrf = random_secret()?;
    let original_id = session.id.clone();
    let mut fresh = DashboardSession {
        id: digest(&value),
        csrf,
        must_change_password: false,
        ..session
    };
    let mut tx = app.db.begin().await?;
    // The session must still exist after expensive hashing; resets and logout invalidate it.
    let result=match role {
        Role::Admin=>sqlx::query("UPDATE admin SET password_hash=?,must_change_password=0 WHERE id=1 AND password_hash=? AND EXISTS(SELECT 1 FROM sessions WHERE id=?)").bind(&hash).bind(&previous).bind(&original_id).execute(&mut *tx).await?,
        Role::User=>sqlx::query("UPDATE users SET password_hash=?,must_change_password=0 WHERE id=? AND enabled=1 AND password_hash=? AND EXISTS(SELECT 1 FROM user_sessions WHERE id=? AND user_id=users.id)").bind(&hash).bind(&fresh.user_id).bind(&previous).bind(&original_id).execute(&mut *tx).await?,
    };
    if result.rows_affected() != 1 {
        return Err(Error::unauthorized());
    }
    match role {
        Role::Admin => {
            sqlx::query("DELETE FROM sessions")
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO sessions(id,csrf,created_at) VALUES(?,?,?)")
                .bind(&fresh.id)
                .bind(&fresh.csrf)
                .bind(app.now())
                .execute(&mut *tx)
                .await?;
        }
        Role::User => {
            sqlx::query("DELETE FROM user_sessions WHERE user_id=?")
                .bind(&fresh.user_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO user_sessions(id,user_id,csrf,created_at) VALUES(?,?,?,?)")
                .bind(&fresh.id)
                .bind(&fresh.user_id)
                .bind(&fresh.csrf)
                .bind(app.now())
                .execute(&mut *tx)
                .await?;
            fresh.username = sqlx::query_scalar("SELECT username FROM users WHERE id=?")
                .bind(&fresh.user_id)
                .fetch_one(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(session_response(&app, role, &value, &fresh).await)
}
pub async fn user_password(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
    Json(input): Json<PasswordInput>,
) -> Result<Response> {
    change_password(app, session, input, Role::User).await
}
