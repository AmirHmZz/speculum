pub mod cargo;
pub mod npm;
pub mod pypi;
use crate::{App, cache, error::Result};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, Method},
    response::Response,
};
use std::sync::Arc;

pub fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}
pub fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
pub async fn file(
    State(app): State<Arc<App>>,
    Path((kind, id, filename)): Path<(String, String, String)>,
    headers: HeaderMap,
    method: Method,
    identity: Option<axum::Extension<crate::auth::MirrorIdentity>>,
) -> Result<Response> {
    cache::serve(
        app,
        &kind,
        &id,
        Some(&filename),
        headers,
        method,
        identity.map(|identity| identity.0.0),
    )
    .await
}
pub fn artifact_link(runtime: &crate::app::Runtime, item: &cache::Artifact) -> String {
    format!(
        "{}/{}/files/{}/{}",
        runtime.settings.public_base_url.trim_end_matches('/'),
        item.registry,
        item.id,
        encode(&item.filename)
    )
}
