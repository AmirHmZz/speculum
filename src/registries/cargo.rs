use super::join;
use crate::{
    App,
    cache::{self, ArtifactInput},
    error::{Error, Result},
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, Method},
    response::Response,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

fn index_path(name: &str) -> Result<String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(Error::bad("Invalid crate name"));
    }
    let name = name.to_ascii_lowercase();
    Ok(match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    })
}
pub async fn config(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response> {
    let runtime = app.snapshot().await;
    Ok(cache::metadata_response(serde_json::to_vec(&json!({"dl":format!("{}/cargo/crates/{{crate}}/{{version}}/download",runtime.settings.public_base_url.trim_end_matches('/')),"auth-required":runtime.settings.registries["cargo"].auth_required})).unwrap(),"application/json",&headers,&method))
}
pub async fn index(
    State(app): State<Arc<App>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    method: Method,
) -> Result<Response> {
    let name = path.rsplit('/').next().ok_or_else(Error::not_found)?;
    if path != index_path(name)? {
        return Err(Error::not_found());
    }
    let _maintenance = app.maintenance.read().await;
    let runtime = app.snapshot().await;
    let (bytes, _) = cache::metadata(
        &app,
        &runtime,
        "cargo",
        &join(&runtime.settings.registries["cargo"].upstream_url, &path),
        "text/plain",
    )
    .await?;
    app.package("cargo", name).await?;
    Ok(cache::metadata_response(
        bytes,
        "text/plain",
        &headers,
        &method,
    ))
}
pub async fn download(
    State(app): State<Arc<App>>,
    Path((name, version)): Path<(String, String)>,
    headers: HeaderMap,
    method: Method,
    identity: Option<axum::Extension<crate::auth::MirrorIdentity>>,
) -> Result<Response> {
    let path = index_path(&name)?;
    if version.len() > 128
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
    {
        return Err(Error::bad("Invalid crate version"));
    }
    let runtime = app.snapshot().await;
    let (bytes, _) = cache::metadata(
        &app,
        &runtime,
        "cargo",
        &join(&runtime.settings.registries["cargo"].upstream_url, &path),
        "text/plain",
    )
    .await?;
    let entry = std::str::from_utf8(&bytes)
        .map_err(|_| Error::upstream("Invalid crate index"))?
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|row| row["vers"].as_str() == Some(&version))
        .ok_or_else(Error::not_found)?;
    let (config, _) = cache::metadata(
        &app,
        &runtime,
        "cargo",
        &join(
            &runtime.settings.registries["cargo"].upstream_url,
            "config.json",
        ),
        "application/json",
    )
    .await?;
    let config: Value = serde_json::from_slice(&config)
        .map_err(|_| Error::upstream("Invalid upstream Cargo config"))?;
    let template = config["dl"]
        .as_str()
        .ok_or_else(|| Error::upstream("Missing Cargo download URL"))?;
    let checksum = entry["cksum"]
        .as_str()
        .ok_or_else(|| Error::upstream("Missing crate checksum"))?;
    let download_name = entry["name"].as_str().unwrap_or(&name);
    let canonical_path = index_path(download_name)?;
    let lower_prefix = canonical_path.rsplit_once('/').unwrap().0;
    let prefix = match download_name.len() {
        1 => "1".into(),
        2 => "2".into(),
        3 => format!("3/{}", &download_name[..1]),
        _ => format!("{}/{}", &download_name[..2], &download_name[2..4]),
    };
    let url = if template.contains('{') {
        template
            .replace("{crate}", download_name)
            .replace("{version}", &version)
            .replace("{prefix}", &prefix)
            .replace("{lowerprefix}", lower_prefix)
            .replace("{sha256-checksum}", checksum)
    } else {
        format!(
            "{}/{download_name}/{version}/download",
            template.trim_end_matches('/')
        )
    };
    let item = {
        let _maintenance = app.maintenance.read().await;
        cache::register(
            &app,
            &runtime,
            ArtifactInput {
                registry: "cargo",
                package: &name.to_ascii_lowercase(),
                version: &version,
                filename: &format!("{download_name}-{version}.crate"),
                url: &url,
                checksums: BTreeMap::from([("sha256".into(), checksum.into())]),
                sidecar: false,
            },
        )
        .await?
    };
    cache::serve(
        app,
        "cargo",
        &item.id,
        None,
        headers,
        method,
        identity.map(|identity| identity.0.0),
    )
    .await
}
