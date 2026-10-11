use crate::{
    App, auth, cache,
    config::Settings,
    error::{Error, Result},
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;

pub async fn settings(State(app): State<Arc<App>>) -> Json<Settings> {
    Json(app.snapshot().await.settings.redacted())
}
pub async fn update_settings(
    State(app): State<Arc<App>>,
    Json(settings): Json<Settings>,
) -> Result<StatusCode> {
    app.save_settings(settings).await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn update_admin(
    State(app): State<Arc<App>>,
    Extension(session): Extension<auth::DashboardSession>,
    Json(input): Json<auth::PasswordInput>,
) -> Result<Response> {
    auth::change_password(app, session, input, auth::Role::Admin).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInput {
    pub username: String,
    pub enabled: bool,
}
pub async fn users(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    let rows = sqlx::query(
        "SELECT id,username,enabled,created_at,must_change_password FROM users ORDER BY username",
    )
    .fetch_all(&app.db)
    .await?;
    let mut users = Vec::new();
    for row in rows {
        let id: String = row.get("id");
        let tokens = sqlx::query("SELECT id,name,created_at FROM tokens WHERE user_id=? ORDER BY created_at").bind(&id).fetch_all(&app.db).await?.into_iter().map(|r| json!({"id":r.get::<String,_>("id"),"name":r.get::<String,_>("name"),"created_at":r.get::<i64,_>("created_at")})).collect::<Vec<_>>();
        users.push(json!({"id":id,"username":row.get::<String,_>("username"),"enabled":row.get::<bool,_>("enabled"),"created_at":row.get::<i64,_>("created_at"),"must_change_password":row.get::<bool,_>("must_change_password"),"tokens":tokens}));
    }
    Ok(Json(json!(users)))
}
pub async fn create_user(
    State(app): State<Arc<App>>,
    Json(input): Json<UserInput>,
) -> Result<(StatusCode, Json<Value>)> {
    auth::validate_username(&input.username)?;
    let id = uuid::Uuid::new_v4().to_string();
    let password = auth::random_secret()?;
    let hash = auth::hash_password(password.clone()).await?;
    sqlx::query(
        "INSERT INTO users(id,username,password_hash,enabled,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(&id)
    .bind(input.username)
    .bind(hash)
    .bind(input.enabled)
    .bind(app.now())
    .execute(&app.db)
    .await
    .map_err(user_error)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":id,"temporary_password":password})),
    ))
}
fn user_error(error: sqlx::Error) -> Error {
    if error
        .as_database_error()
        .is_some_and(|e| e.is_unique_violation())
    {
        Error(StatusCode::CONFLICT, "Username already exists".into())
    } else {
        error.into()
    }
}
pub async fn update_user(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(input): Json<UserInput>,
) -> Result<StatusCode> {
    auth::validate_username(&input.username)?;
    let mut tx = app.db.begin().await?;
    let result = sqlx::query("UPDATE users SET username=?,enabled=? WHERE id=?")
        .bind(input.username)
        .bind(input.enabled)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(user_error)?;
    if result.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    if !input.enabled {
        sqlx::query("DELETE FROM user_sessions WHERE user_id=?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
pub async fn reset_user_password(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let password = auth::random_secret()?;
    let hash = auth::hash_password(password.clone()).await?;
    let mut tx = app.db.begin().await?;
    let result = sqlx::query("UPDATE users SET password_hash=?,must_change_password=1 WHERE id=?")
        .bind(hash)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    sqlx::query("DELETE FROM user_sessions WHERE user_id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"temporary_password":password})))
}
pub async fn delete_user(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let result = sqlx::query("DELETE FROM users WHERE id=?")
        .bind(id)
        .execute(&app.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenInput {
    pub name: String,
}
pub async fn create_token(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Json(input): Json<TokenInput>,
) -> Result<Json<Value>> {
    if input.name.trim().is_empty() || input.name.len() > 100 {
        return Err(Error::bad("Token name must contain 1–100 characters"));
    }
    if sqlx::query("SELECT id FROM users WHERE id=?")
        .bind(&id)
        .fetch_optional(&app.db)
        .await?
        .is_none()
    {
        return Err(Error::not_found());
    }
    let token = format!("spm_{}", auth::random_secret()?);
    let token_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tokens(id,user_id,name,prefix,hash,created_at) VALUES(?,?,?,?,?,?)")
        .bind(&token_id)
        .bind(id)
        .bind(input.name)
        .bind(&token[..12])
        .bind(auth::digest(&token))
        .bind(app.now())
        .execute(&app.db)
        .await?;
    Ok(Json(json!({"id":token_id,"token":token})))
}
pub async fn delete_token(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    sqlx::query("DELETE FROM tokens WHERE id=?")
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Default, Serialize)]
struct Totals {
    packages: i64,
    versions: i64,
    files: i64,
    stored_bytes: i64,
    requests: i64,
    hits: i64,
    misses: i64,
    upstream_downloads: i64,
    failures: i64,
    upstream_bytes: i64,
    client_bytes: i64,
    last_access: Option<i64>,
}
fn package_json(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"registry":row.get::<String,_>("registry"),"name":row.get::<String,_>("name"),
        "versions":row.get::<i64,_>("versions"),"files":row.get::<i64,_>("files"),"stored_bytes":row.get::<i64,_>("stored_bytes"),
        "requests":row.get::<i64,_>("requests"),"hits":row.get::<i64,_>("hits"),"misses":row.get::<i64,_>("misses"),
        "upstream_downloads":row.get::<i64,_>("upstream_downloads"),"failures":row.get::<i64,_>("failures"),"upstream_bytes":row.get::<i64,_>("upstream_bytes"),"client_bytes":row.get::<i64,_>("client_bytes"),"last_access":row.get::<Option<i64>,_>("last_access")})
}
const PACKAGE_SELECT: &str = "SELECT p.*, (SELECT COUNT(DISTINCT version) FROM artifacts WHERE package_id=p.id AND downloaded_at IS NOT NULL AND is_sidecar=0) AS versions,(SELECT COUNT(*) FROM artifacts WHERE package_id=p.id AND downloaded_at IS NOT NULL) AS files, (SELECT COALESCE(SUM(size),0) FROM artifacts WHERE package_id=p.id AND downloaded_at IS NOT NULL) AS stored_bytes FROM packages p";
fn add(totals: &mut Totals, row: &sqlx::sqlite::SqliteRow) {
    totals.packages += i64::from(row.get::<i64, _>("files") > 0);
    totals.versions += row.get::<i64, _>("versions");
    totals.files += row.get::<i64, _>("files");
    totals.stored_bytes += row.get::<i64, _>("stored_bytes");
    totals.requests += row.get::<i64, _>("requests");
    totals.hits += row.get::<i64, _>("hits");
    totals.misses += row.get::<i64, _>("misses");
    totals.upstream_downloads += row.get::<i64, _>("upstream_downloads");
    totals.failures += row.get::<i64, _>("failures");
    totals.upstream_bytes += row.get::<i64, _>("upstream_bytes");
    totals.client_bytes += row.get::<i64, _>("client_bytes");
    totals.last_access = totals.last_access.max(row.get("last_access"));
}
pub async fn summary(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    let rows = sqlx::query(PACKAGE_SELECT).fetch_all(&app.db).await?;
    let mut total = Totals::default();
    let mut registries =
        std::collections::BTreeMap::from(crate::config::REGISTRIES.map(|k| (k, Totals::default())));
    for row in &rows {
        add(&mut total, row);
        if let Some(totals) = registries.get_mut(row.get::<String, _>("registry").as_str()) {
            add(totals, row);
        }
    }
    Ok(Json(
        json!({"total":total,"registries":registries,"known_packages":rows.len()}),
    ))
}
#[derive(Deserialize)]
pub struct PackageQuery {
    pub registry: Option<String>,
    pub q: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}
pub async fn packages(
    State(app): State<Arc<App>>,
    Query(query): Query<PackageQuery>,
) -> Result<Json<Value>> {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.page_size.unwrap_or(25).clamp(1, 100);
    let registry = query.registry.filter(|s| !s.is_empty());
    let search = format!("%{}%", query.q.unwrap_or_default());
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM packages WHERE (? IS NULL OR registry=?) AND name LIKE ?",
    )
    .bind(&registry)
    .bind(&registry)
    .bind(&search)
    .fetch_one(&app.db)
    .await?;
    let sql = format!(
        "{PACKAGE_SELECT} WHERE (? IS NULL OR p.registry=?) AND p.name LIKE ? ORDER BY p.name,p.registry LIMIT ? OFFSET ?"
    );
    let rows = sqlx::query(&sql)
        .bind(&registry)
        .bind(&registry)
        .bind(search)
        .bind(i64::from(limit))
        .bind(i64::from(page - 1) * i64::from(limit))
        .fetch_all(&app.db)
        .await?;
    Ok(Json(
        json!({"items":rows.iter().map(package_json).collect::<Vec<_>>(),"total":count,"page":page,"page_size":limit}),
    ))
}
pub async fn package(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Value>> {
    let row = sqlx::query(&format!("{PACKAGE_SELECT} WHERE p.id=?"))
        .bind(&id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(Error::not_found)?;
    let runtime = app.snapshot().await;
    let kind: String = row.get("registry");
    let ttl = runtime.settings.registries[&kind].cache_lifetime_seconds;
    let artifacts = sqlx::query("SELECT id,version,filename,size,downloaded_at,is_sidecar,upstream_identity FROM artifacts WHERE package_id=? AND downloaded_at IS NOT NULL ORDER BY version,filename").bind(id).fetch_all(&app.db).await?.iter().map(|r| { let at:Option<i64> = r.get("downloaded_at"); json!({"id":r.get::<String,_>("id"),"version":r.get::<String,_>("version"),"filename":r.get::<String,_>("filename"),"size":r.get::<i64,_>("size"),"downloaded_at":at,"expires_at":at.and_then(|at| ttl.map(|ttl| at.saturating_add(ttl as i64))),"is_sidecar":r.get::<bool,_>("is_sidecar"),"upstream":r.get::<String,_>("upstream_identity")}) }).collect::<Vec<_>>();
    Ok(Json(
        json!({"package":package_json(&row),"artifacts":artifacts}),
    ))
}
pub async fn delete_artifact(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let _maintenance = app.maintenance.read().await;
    cache::evict(&app, &id, false).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
pub struct DeletePackage {
    pub version: Option<String>,
}
pub async fn delete_package(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(query): Query<DeletePackage>,
) -> Result<StatusCode> {
    let _maintenance = app.maintenance.write().await;
    if sqlx::query("SELECT id FROM packages WHERE id=?")
        .bind(&id)
        .fetch_optional(&app.db)
        .await?
        .is_none()
    {
        return Err(Error::not_found());
    }
    let rows =
        sqlx::query("SELECT id FROM artifacts WHERE package_id=? AND (? IS NULL OR version=?)")
            .bind(id)
            .bind(&query.version)
            .bind(&query.version)
            .fetch_all(&app.db)
            .await?;
    for row in rows {
        cache::evict(&app, &row.get::<String, _>("id"), false).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
pub async fn clear_registry(
    State(app): State<Arc<App>>,
    Path(kind): Path<String>,
) -> Result<StatusCode> {
    if !crate::config::REGISTRIES.contains(&kind.as_str()) {
        return Err(Error::not_found());
    }
    let _maintenance = app.maintenance.write().await;
    for row in
        sqlx::query("SELECT id FROM artifacts WHERE registry=? AND downloaded_at IS NOT NULL")
            .bind(&kind)
            .fetch_all(&app.db)
            .await?
    {
        cache::evict(&app, &row.get::<String, _>("id"), false).await?;
    }
    sqlx::query("DELETE FROM metadata WHERE registry=?")
        .bind(kind)
        .execute(&app.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
