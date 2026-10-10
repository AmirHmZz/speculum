use super::{artifact_link, encode, join};
use crate::{
    App,
    cache::{self, ArtifactInput},
    error::{Error, Result},
};
use axum::{
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

pub async fn metadata(
    State(app): State<Arc<App>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    method: Method,
) -> Result<Response> {
    let _maintenance = app.maintenance.read().await;
    let path = percent_encoding::percent_decode_str(uri.path().strip_prefix("/npm/").unwrap_or(""))
        .decode_utf8()
        .map_err(|_| Error::bad("Invalid package encoding"))?;
    if path == "-/ping" {
        return Ok(cache::metadata_response(
            b"{}".to_vec(),
            "application/json",
            &headers,
            &method,
        ));
    }
    let segments: Vec<&str> = path.split('/').collect();
    let (name, version) = if path.starts_with('@') {
        if !(2..=3).contains(&segments.len()) {
            return Err(Error::not_found());
        }
        (
            format!("{}/{}", segments[0], segments[1]),
            segments.get(2).copied(),
        )
    } else {
        if !(1..=2).contains(&segments.len()) {
            return Err(Error::not_found());
        }
        (segments[0].to_owned(), segments.get(1).copied())
    };
    if name.is_empty()
        || name.len() > 214
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@/._-~".contains(&b))
    {
        return Err(Error::bad("Invalid npm package name"));
    }
    let runtime = app.snapshot().await;
    let accept = if headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("application/vnd.npm.install-v1+json"))
    {
        "application/vnd.npm.install-v1+json"
    } else {
        "application/json"
    };
    let url = join(
        &runtime.settings.registries["npm"].upstream_url,
        &encode(&name),
    );
    let (bytes, _) = cache::metadata(&app, &runtime, "npm", &url, accept).await?;
    let mut document: Value =
        serde_json::from_slice(&bytes).map_err(|_| Error::upstream("Invalid npm metadata"))?;
    let versions = document
        .get_mut("versions")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| Error::upstream("Missing npm versions"))?;
    for (version, data) in versions {
        let dist = data
            .get_mut("dist")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| Error::upstream("Missing npm distribution"))?;
        let tarball = dist
            .get("tarball")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::upstream("Missing npm tarball"))?
            .to_owned();
        let mut hashes = dist
            .get("integrity")
            .and_then(Value::as_str)
            .map(cache::sri_checksums)
            .unwrap_or_default();
        if hashes.is_empty()
            && let Some(hash) = dist.get("shasum").and_then(Value::as_str)
        {
            hashes = BTreeMap::from([("sha1".into(), hash.into())]);
        }
        let filename = format!("{}-{version}.tgz", name.rsplit('/').next().unwrap());
        let item = cache::register(
            &app,
            &runtime,
            ArtifactInput {
                registry: "npm",
                package: &name,
                version,
                filename: &filename,
                url: &tarball,
                checksums: hashes,
                sidecar: false,
            },
        )
        .await?;
        dist.insert("tarball".into(), json!(artifact_link(&runtime, &item)));
    }
    app.package("npm", &name).await?;
    if let Some(version) = version {
        let resolved = document
            .get("dist-tags")
            .and_then(|tags| tags.get(version))
            .and_then(Value::as_str)
            .unwrap_or(version);
        let data = document["versions"]
            .get(resolved)
            .ok_or_else(Error::not_found)?;
        return Ok(cache::metadata_response(
            serde_json::to_vec(data).unwrap(),
            "application/json",
            &headers,
            &method,
        ));
    }
    Ok(cache::metadata_response(
        serde_json::to_vec(&document).unwrap(),
        accept,
        &headers,
        &method,
    ))
}
pub async fn audit(
    State(app): State<Arc<App>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    let runtime = app.snapshot().await;
    let mut forwarded = HeaderMap::new();
    for name in ["content-type", "content-encoding", "accept"] {
        if let Some(value) = headers.get(name) {
            forwarded.insert(name, value.clone());
        }
    }
    let url = join(
        &runtime.settings.registries["npm"].upstream_url,
        uri.path().strip_prefix("/npm/").unwrap(),
    );
    let response =
        cache::upstream(&runtime, "npm", Method::POST, &url, forwarded, Some(body)).await?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| "application/json".parse().unwrap());
    let bytes = response
        .bytes()
        .await
        .map_err(|_| Error::upstream("Interrupted audit response"))?;
    let mut result = Response::new(Body::from(bytes));
    *result.status_mut() = status;
    result.headers_mut().insert("content-type", content_type);
    Ok(result)
}
pub async fn root() -> (StatusCode, axum::Json<Value>) {
    (
        StatusCode::OK,
        axum::Json(json!({"name":"Speculum","read_only":true})),
    )
}
