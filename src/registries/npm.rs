use super::{artifact_link, encode, join};
use crate::{
    App,
    app::Runtime,
    cache::{self, Artifact, ArtifactInput},
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
    identity: Option<axum::Extension<crate::auth::MirrorIdentity>>,
) -> Result<Response> {
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
    let (name, tail) = if path.starts_with('@') {
        if segments.len() < 2 || segments[0].len() < 2 || segments[1].is_empty() {
            return Err(Error::not_found());
        }
        (format!("{}/{}", segments[0], segments[1]), &segments[2..])
    } else {
        (segments[0].to_owned(), &segments[1..])
    };
    if name.is_empty()
        || name.len() > 214
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"@/._-~".contains(&b))
    {
        return Err(Error::bad("Invalid npm package name"));
    }
    let version = match tail {
        ["-", filename] => {
            return download(
                app,
                &name,
                filename,
                headers,
                method,
                identity.map(|identity| identity.0.0),
            )
            .await;
        }
        [] => None,
        [version] => Some(*version),
        _ => return Err(Error::not_found()),
    };
    let _maintenance = app.maintenance.read().await;
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
        let filename = format!("{}-{version}.tgz", name.rsplit('/').next().unwrap());
        let item = register_dist(&app, &runtime, &name, version, &filename, dist).await?;
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

async fn register_dist(
    app: &App,
    runtime: &Runtime,
    name: &str,
    version: &str,
    filename: &str,
    dist: &serde_json::Map<String, Value>,
) -> Result<Artifact> {
    let tarball = dist
        .get("tarball")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::upstream("Missing npm tarball"))?;
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
    cache::register(
        app,
        runtime,
        ArtifactInput {
            registry: "npm",
            package: name,
            version,
            filename,
            url: tarball,
            checksums: hashes,
            sidecar: false,
        },
    )
    .await
}

async fn download(
    app: Arc<App>,
    name: &str,
    filename: &str,
    headers: HeaderMap,
    method: Method,
    user_id: Option<String>,
) -> Result<Response> {
    let version = filename
        .strip_prefix(name.rsplit('/').next().unwrap())
        .and_then(|suffix| suffix.strip_prefix('-'))
        .and_then(|suffix| suffix.strip_suffix(".tgz"))
        .filter(|version| {
            !version.is_empty()
                && version.len() <= 128
                && version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
        })
        .ok_or_else(Error::not_found)?;
    let id = {
        let _maintenance = app.maintenance.read().await;
        let runtime = app.snapshot().await;
        // Use the same artifact as rewritten metadata URLs, including for offline installs.
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT a.id FROM artifacts a JOIN packages p ON p.id=a.package_id \
             WHERE a.registry='npm' AND a.upstream_identity=? AND p.name=? \
             AND a.version=? AND a.filename=? AND a.is_sidecar=0",
        )
        .bind(&runtime.settings.registries["npm"].upstream_url)
        .bind(name)
        .bind(version)
        .bind(filename)
        .fetch_optional(&app.db)
        .await?;
        if let Some(id) = existing {
            id
        } else {
            let url = join(
                &runtime.settings.registries["npm"].upstream_url,
                &encode(name),
            );
            let (bytes, _) = cache::metadata(
                &app,
                &runtime,
                "npm",
                &url,
                "application/vnd.npm.install-v1+json",
            )
            .await?;
            let document: Value = serde_json::from_slice(&bytes)
                .map_err(|_| Error::upstream("Invalid npm metadata"))?;
            let data = document
                .get("versions")
                .and_then(Value::as_object)
                .ok_or_else(|| Error::upstream("Missing npm versions"))?
                .get(version)
                .ok_or_else(Error::not_found)?;
            let dist = data
                .get("dist")
                .and_then(Value::as_object)
                .ok_or_else(|| Error::upstream("Missing npm distribution"))?;
            register_dist(&app, &runtime, name, version, filename, dist)
                .await?
                .id
        }
    };
    cache::serve(app, "npm", &id, Some(filename), headers, method, user_id).await
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
