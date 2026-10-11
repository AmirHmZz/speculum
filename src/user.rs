use crate::{
    App,
    admin::{PackageQuery, TokenInput},
    auth::{self, DashboardSession},
    error::{Error, Result},
    stats,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;

pub async fn tokens(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
) -> Result<Json<Value>> {
    let rows = sqlx::query(
        "SELECT id,name,prefix,created_at FROM tokens WHERE user_id=? ORDER BY created_at,id",
    )
    .bind(session.user_id)
    .fetch_all(&app.db)
    .await?;
    Ok(Json(json!(rows.iter().map(|row|json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"prefix":row.get::<String,_>("prefix"),"created_at":row.get::<i64,_>("created_at")})).collect::<Vec<_>>())))
}
pub async fn create_token(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
    Json(input): Json<TokenInput>,
) -> Result<Json<Value>> {
    if input.name.trim().is_empty() || input.name.len() > 100 {
        return Err(Error::bad("Token name must contain 1–100 characters"));
    }
    let value = format!("spm_{}", auth::random_secret()?);
    let id = uuid::Uuid::new_v4().to_string();
    let result=sqlx::query("INSERT INTO tokens(id,user_id,name,prefix,hash,created_at) SELECT ?,u.id,?,?,?,? FROM users u WHERE u.id=? AND u.enabled=1 AND u.must_change_password=0 AND EXISTS(SELECT 1 FROM user_sessions s WHERE s.id=? AND s.user_id=u.id)").bind(&id).bind(input.name).bind(&value[..12]).bind(auth::digest(&value)).bind(app.now()).bind(&session.user_id).bind(&session.id).execute(&app.db).await?;
    if result.rows_affected() == 0 {
        return Err(Error::unauthorized());
    }
    Ok(Json(json!({"id":id,"token":value})))
}
pub async fn delete_token(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let result = sqlx::query("DELETE FROM tokens WHERE id=? AND user_id=?")
        .bind(id)
        .bind(session.user_id)
        .execute(&app.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(Error::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}
pub async fn summary(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
) -> Result<Json<Value>> {
    Ok(Json(
        stats::summary(&app, session.user_id.as_deref().unwrap()).await?,
    ))
}
pub async fn packages(
    State(app): State<Arc<App>>,
    Extension(session): Extension<DashboardSession>,
    Query(query): Query<PackageQuery>,
) -> Result<Json<Value>> {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.page_size.unwrap_or(25).clamp(1, 100);
    let registry = query.registry.filter(|v| !v.is_empty());
    let search = format!("%{}%", query.q.unwrap_or_default());
    let filter = " WHERE s.user_id=? AND (? IS NULL OR p.registry=?) AND p.name LIKE ?";
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM user_package_stats s JOIN packages p ON p.id=s.package_id{filter}"
    ))
    .bind(&session.user_id)
    .bind(&registry)
    .bind(&registry)
    .bind(&search)
    .fetch_one(&app.db)
    .await?;
    let rows = sqlx::query(&format!(
        "{}{filter} ORDER BY p.name,p.registry LIMIT ? OFFSET ?",
        stats::USER_SELECT
    ))
    .bind(session.user_id)
    .bind(&registry)
    .bind(&registry)
    .bind(search)
    .bind(i64::from(limit))
    .bind(i64::from(page - 1) * i64::from(limit))
    .fetch_all(&app.db)
    .await?;
    Ok(Json(
        json!({"items":rows.iter().map(stats::package_json).collect::<Vec<_>>(),"total":count,"page":page,"page_size":limit}),
    ))
}
pub async fn config(State(app): State<Arc<App>>) -> Json<Value> {
    let settings = app.snapshot().await.settings;
    let registries = settings
        .registries
        .iter()
        .map(|(kind, config)| {
            (
                kind.clone(),
                json!({"enabled":config.enabled,"auth_required":config.auth_required}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    Json(json!({"public_base_url":settings.public_base_url,"registries":registries}))
}
