use crate::{
    App,
    app::Runtime,
    error::{Error, Result},
};
use axum::{
    body::Body,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, TryStreamExt};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use sqlx::Row;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::OwnedRwLockReadGuard,
};
use tokio_util::io::ReaderStream;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub package_id: String,
    pub registry: String,
    pub upstream_identity: String,
    pub version: String,
    pub filename: String,
    pub url: String,
    pub checksums: BTreeMap<String, String>,
    pub downloaded_at: Option<i64>,
    pub size: i64,
    pub content_type: Option<String>,
    pub is_sidecar: bool,
}
impl Artifact {
    fn from_row(row: sqlx::sqlite::SqliteRow) -> Result<Self> {
        Ok(Self {
            id: row.get("id"),
            package_id: row.get("package_id"),
            registry: row.get("registry"),
            upstream_identity: row.get("upstream_identity"),
            version: row.get("version"),
            filename: row.get("filename"),
            url: row.get("url"),
            checksums: serde_json::from_str(&row.get::<String, _>("checksums"))
                .map_err(|_| Error::bad("Invalid artifact checksums"))?,
            downloaded_at: row.get("downloaded_at"),
            size: row.get("size"),
            content_type: row.get("content_type"),
            is_sidecar: row.get("is_sidecar"),
        })
    }
    pub fn expires_at(&self, runtime: &Runtime) -> Option<i64> {
        self.downloaded_at.and_then(|at| {
            runtime.settings.registries[&self.registry]
                .cache_lifetime_seconds
                .map(|ttl| at.saturating_add(ttl as i64))
        })
    }
    pub fn valid(&self, runtime: &Runtime, now: i64) -> bool {
        self.downloaded_at.is_some() && self.expires_at(runtime).is_none_or(|at| at > now)
    }
    pub fn path(&self, app: &App) -> PathBuf {
        app.dir.join("artifacts").join(&self.id)
    }
}
pub async fn artifact(app: &App, id: &str) -> Result<Artifact> {
    Artifact::from_row(
        sqlx::query("SELECT * FROM artifacts WHERE id=?")
            .bind(id)
            .fetch_optional(&app.db)
            .await?
            .ok_or_else(Error::not_found)?,
    )
}
pub struct ArtifactInput<'a> {
    pub registry: &'a str,
    pub package: &'a str,
    pub version: &'a str,
    pub filename: &'a str,
    pub url: &'a str,
    pub checksums: BTreeMap<String, String>,
    pub sidecar: bool,
}
pub async fn register(app: &App, runtime: &Runtime, input: ArtifactInput<'_>) -> Result<Artifact> {
    let ArtifactInput {
        registry,
        package,
        version,
        filename,
        url,
        checksums,
        sidecar,
    } = input;
    if filename.is_empty() || filename.contains(['/', '\\', '\0', '\r', '\n']) {
        return Err(Error::upstream("Invalid upstream filename"));
    }
    validate_url(runtime, registry, url)?;
    let identity = &runtime.settings.registries[registry].upstream_url;
    let key = crate::auth::digest(
        &serde_json::to_string(&(registry, identity, package, version, filename)).unwrap(),
    );
    let package_id = app.package(registry, package).await?;
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO artifacts(id,key,package_id,registry,upstream_identity,version,filename,url,checksums,is_sidecar) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(key) DO UPDATE SET url=excluded.url,checksums=excluded.checksums")
        .bind(&id).bind(&key).bind(package_id).bind(registry).bind(identity).bind(version).bind(filename).bind(url).bind(serde_json::to_string(&checksums).unwrap()).bind(sidecar).execute(&app.db).await?;
    Artifact::from_row(
        sqlx::query("SELECT * FROM artifacts WHERE key=?")
            .bind(key)
            .fetch_one(&app.db)
            .await?,
    )
}
pub fn validate_url(runtime: &Runtime, kind: &str, value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| Error::upstream("Invalid upstream URL"))?;
    let config = &runtime.settings.registries[kind];
    let upstream = Url::parse(&config.upstream_url)
        .map_err(|_| Error::upstream("Invalid upstream configuration"))?;
    let allowed = url.host_str().is_some_and(|host| {
        Some(host) == upstream.host_str()
            || config
                .allowed_artifact_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host))
    });
    if !matches!(url.scheme(), "http" | "https")
        || !allowed
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::upstream(
            "Upstream URL host is not allowed; update allowed artifact hosts in Settings",
        ));
    }
    Ok(url)
}
pub async fn upstream(
    runtime: &Runtime,
    kind: &str,
    method: Method,
    url: &str,
    headers: HeaderMap,
    body: Option<bytes::Bytes>,
) -> Result<reqwest::Response> {
    let mut url = validate_url(runtime, kind, url)?;
    for _ in 0..6 {
        let mut request = runtime.clients[kind]
            .request(method.clone(), url.clone())
            .headers(headers.clone());
        if let Some(body) = &body {
            request = request.body(body.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|_| Error::upstream("Unable to reach upstream"))?;
        if response.status().is_redirection() && response.status() != StatusCode::NOT_MODIFIED {
            let target = response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .ok_or_else(|| Error::upstream("Invalid upstream redirect"))?;
            url = validate_url(
                runtime,
                kind,
                url.join(target)
                    .map_err(|_| Error::upstream("Invalid upstream redirect"))?
                    .as_str(),
            )?;
            continue;
        }
        return Ok(response);
    }
    Err(Error::upstream("Too many upstream redirects"))
}
pub async fn metadata(
    app: &App,
    runtime: &Runtime,
    kind: &str,
    url: &str,
    accept: &str,
) -> Result<(Vec<u8>, String)> {
    let key = crate::auth::digest(&format!("{kind}\n{url}\n{accept}"));
    let _lock = app
        .artifact_lock(&format!("metadata:{key}"))
        .write_owned()
        .await;
    let cached = sqlx::query("SELECT * FROM metadata WHERE key=?")
        .bind(&key)
        .fetch_optional(&app.db)
        .await?;
    if let Some(row) = &cached
        && app.now().saturating_sub(row.get::<i64, _>("fetched_at"))
            < runtime.settings.registries[kind].metadata_freshness_seconds as i64
    {
        return Ok((row.get("body"), row.get("content_type")));
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        "accept",
        accept
            .parse()
            .map_err(|_| Error::bad("Invalid Accept header"))?,
    );
    if let Some(row) = &cached {
        if let Some(etag) = row.get::<Option<String>, _>("etag")
            && let Ok(value) = etag.parse()
        {
            headers.insert("if-none-match", value);
        }
        if let Some(modified) = row.get::<Option<String>, _>("last_modified")
            && let Ok(value) = modified.parse()
        {
            headers.insert("if-modified-since", value);
        }
    }
    let fetched = async {
        let response = upstream(runtime, kind, Method::GET, url, headers, None).await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            let row = cached.as_ref().ok_or_else(|| Error::upstream("Unexpected upstream 304"))?;
            sqlx::query("UPDATE metadata SET fetched_at=? WHERE key=?").bind(app.now()).bind(&key).execute(&app.db).await?;
            return Ok((row.get("body"), row.get("content_type")));
        }
        if !response.status().is_success() {
            return Err(if response.status() == StatusCode::NOT_FOUND { Error::not_found() } else { Error::upstream("Upstream metadata request failed") });
        }
        let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_owned();
        let etag = response.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let modified = response.headers().get("last-modified").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let mut bytes = Vec::new(); let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await { let chunk = chunk.map_err(|_| Error::upstream("Interrupted metadata response"))?; if bytes.len() + chunk.len() > 64 * 1024 * 1024 { return Err(Error::upstream("Metadata exceeds 64 MiB limit")); } bytes.extend_from_slice(&chunk); }
        validate_metadata(kind, &content_type, &bytes)?;
        sqlx::query("INSERT INTO metadata(key,registry,body,content_type,etag,last_modified,fetched_at) VALUES(?,?,?,?,?,?,?) ON CONFLICT(key) DO UPDATE SET body=excluded.body,content_type=excluded.content_type,etag=excluded.etag,last_modified=excluded.last_modified,fetched_at=excluded.fetched_at")
            .bind(&key).bind(kind).bind(&bytes).bind(&content_type).bind(etag).bind(modified).bind(app.now()).execute(&app.db).await?;
        Ok((bytes,content_type))
    }.await;
    match fetched {
        Ok(value) => Ok(value),
        Err(error) if error.0 != StatusCode::NOT_FOUND && cached.is_some() => {
            let row = cached.unwrap();
            Ok((row.get("body"), row.get("content_type")))
        }
        Err(error) => Err(error),
    }
}
fn validate_metadata(kind: &str, content_type: &str, bytes: &[u8]) -> Result<()> {
    if (kind == "npm" || content_type.contains("json"))
        && serde_json::from_slice::<serde_json::Value>(bytes).is_err()
    {
        return Err(Error::upstream("Invalid upstream JSON metadata"));
    }
    if kind == "cargo" && !content_type.contains("json") {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| Error::upstream("Invalid Cargo index encoding"))?;
        if text
            .lines()
            .any(|line| serde_json::from_str::<serde_json::Value>(line).is_err())
        {
            return Err(Error::upstream("Invalid Cargo index entries"));
        }
    }
    Ok(())
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
async fn download(
    app: &App,
    runtime: &Runtime,
    artifact: &Artifact,
    user_id: Option<&str>,
) -> Result<()> {
    let _permit = runtime.downloads.acquire().await;
    let path = app.dir.join("tmp").join(uuid::Uuid::new_v4().to_string());
    let temporary = Temporary(path.clone());
    let mut headers = HeaderMap::new();
    headers.insert("accept-encoding", "identity".parse().unwrap());
    let response = upstream(
        runtime,
        &artifact.registry,
        Method::GET,
        &artifact.url,
        headers,
        None,
    )
    .await?;
    if !response.status().is_success() {
        return Err(if response.status() == StatusCode::NOT_FOUND {
            Error::not_found()
        } else {
            Error::upstream("Upstream artifact request failed")
        });
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let expected_length = response.content_length();
    let mut file = tokio::fs::File::create(&path).await?;
    let (mut sha256, mut sha512, mut sha1) = (Sha256::new(), Sha512::new(), Sha1::new());
    let mut size = 0i64;
    let mut stream = response.bytes_stream();
    crate::stats::record(
        app,
        &artifact.package_id,
        user_id,
        crate::stats::Delta {
            upstream_downloads: 1,
            ..Default::default()
        },
    )
    .await?;
    let result = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Error::upstream("Interrupted artifact download"))?;
            size += chunk.len() as i64;
            sha256.update(&chunk);
            sha512.update(&chunk);
            sha1.update(&chunk);
            file.write_all(&chunk).await?;
        }
        if expected_length.is_some_and(|length| length != size as u64) {
            return Err(Error::upstream("Incomplete artifact download"));
        }
        let actual = BTreeMap::from([
            ("sha256", format!("{:x}", sha256.finalize())),
            ("sha512", format!("{:x}", sha512.finalize())),
            ("sha1", format!("{:x}", sha1.finalize())),
        ]);
        for (algorithm, expected) in &artifact.checksums {
            if let Some(hash) = actual.get(algorithm.as_str())
                && !hash.eq_ignore_ascii_case(expected)
            {
                return Err(Error::upstream("Artifact checksum mismatch"));
            }
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        // A fresh UUID destination makes promotion atomic on Windows as well as Unix.
        tokio::fs::rename(&path, artifact.path(app)).await?;
        sqlx::query("UPDATE artifacts SET downloaded_at=?,size=?,content_type=? WHERE id=?")
            .bind(app.now())
            .bind(size)
            .bind(content_type)
            .bind(&artifact.id)
            .execute(&app.db)
            .await?;
        Ok(())
    }
    .await;
    crate::stats::record(
        app,
        &artifact.package_id,
        user_id,
        crate::stats::Delta {
            upstream_bytes: size,
            ..Default::default()
        },
    )
    .await?;
    drop(temporary);
    result
}
pub fn sri_checksums(value: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    // SRI allows alternatives within an algorithm; npm emits one digest per algorithm.
    for item in value.split_whitespace() {
        if let Some((algorithm, encoded)) = item.split_once('-')
            && matches!(algorithm, "sha256" | "sha512" | "sha1")
            && let Ok(bytes) = STANDARD.decode(encoded.split('?').next().unwrap_or(encoded))
        {
            result.insert(
                algorithm.into(),
                bytes.iter().map(|b| format!("{b:02x}")).collect(),
            );
        }
    }
    result
}
struct Delivery {
    app: Arc<App>,
    package_id: String,
    bytes: i64,
    user_id: Option<String>,
    _lease: OwnedRwLockReadGuard<()>,
}
impl Delivery {
    fn record(&mut self, bytes: usize) {
        self.bytes += bytes as i64;
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        let (app, id, bytes, user_id) = (
            self.app.clone(),
            self.package_id.clone(),
            self.bytes,
            self.user_id.clone(),
        );
        if bytes > 0 {
            self.app.deliveries.spawn(async move {
                if let Err(error) = crate::stats::record(
                    &app,
                    &id,
                    user_id.as_deref(),
                    crate::stats::Delta {
                        client_bytes: bytes,
                        ..Default::default()
                    },
                )
                .await
                {
                    tracing::error!(message=%error.1,"Unable to record delivery bytes");
                }
            });
        }
    }
}
pub fn byte_range(headers: &HeaderMap, size: u64) -> Result<Option<(u64, u64)>> {
    let Some(value) = headers.get("range") else {
        return Ok(None);
    };
    let invalid = || {
        Error(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "Invalid byte range".into(),
        )
    };
    let range = value
        .to_str()
        .map_err(|_| invalid())?
        .strip_prefix("bytes=")
        .ok_or_else(invalid)?;
    if range.contains(',') || size == 0 {
        return Err(invalid());
    }
    let (start, end) = range.split_once('-').ok_or_else(invalid)?;
    let (start, end) = if start.is_empty() {
        let suffix: u64 = end.parse().map_err(|_| invalid())?;
        if suffix == 0 {
            return Err(invalid());
        }
        (size.saturating_sub(suffix), size - 1)
    } else {
        let start: u64 = start.parse().map_err(|_| invalid())?;
        let end = if end.is_empty() {
            size - 1
        } else {
            end.parse::<u64>().map_err(|_| invalid())?.min(size - 1)
        };
        (start, end)
    };
    if start > end || start >= size {
        return Err(invalid());
    }
    Ok(Some((start, end)))
}
fn matches_etag(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|values| {
            values
                .split(',')
                .any(|v| v.trim() == "*" || v.trim().trim_start_matches("W/") == etag)
        })
}
pub fn metadata_response(
    bytes: Vec<u8>,
    content_type: &str,
    headers: &HeaderMap,
    method: &Method,
) -> Response {
    let etag = format!("\"{}\"", crate::auth::digest(&STANDARD.encode(&bytes)));
    let unchanged = matches_etag(headers, &etag);
    let mut response = Response::new(if unchanged || method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes.clone())
    });
    if unchanged {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
    } else {
        response
            .headers_mut()
            .insert("content-length", bytes.len().into());
    }
    response.headers_mut().insert("etag", etag.parse().unwrap());
    response
        .headers_mut()
        .insert("content-type", content_type.parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    response
        .headers_mut()
        .insert("vary", "Accept, Authorization".parse().unwrap());
    response
}
pub async fn serve(
    app: Arc<App>,
    kind: &str,
    id: &str,
    filename: Option<&str>,
    headers: HeaderMap,
    method: Method,
    user_id: Option<String>,
) -> Result<Response> {
    let _maintenance = app.maintenance.read().await;
    let mut item = artifact(&app, id).await?;
    if item.registry != kind || filename.is_some_and(|name| item.filename != name) {
        return Err(Error::not_found());
    }
    let runtime = app.snapshot().await;
    if item.upstream_identity != runtime.settings.registries[kind].upstream_url {
        return Err(Error(
            StatusCode::GONE,
            "Upstream changed; refresh package metadata".into(),
        ));
    }
    let lock = app.artifact_lock(id);
    let mut lease = lock.clone().read_owned().await;
    let mut valid =
        item.valid(&runtime, app.now()) && tokio::fs::try_exists(item.path(&app)).await?;
    if method == Method::HEAD && !valid {
        drop(lease);
        let response = upstream(
            &runtime,
            kind,
            Method::HEAD,
            &item.url,
            HeaderMap::new(),
            None,
        )
        .await?;
        let mut result = Response::new(Body::empty());
        *result.status_mut() = response.status();
        for name in [
            "content-type",
            "content-length",
            "last-modified",
            "etag",
            "accept-ranges",
        ] {
            if let Some(value) = response.headers().get(name) {
                result.headers_mut().insert(name, value.clone());
            }
        }
        result
            .headers_mut()
            .insert("cache-control", "no-cache".parse().unwrap());
        return Ok(result);
    }
    if method == Method::GET {
        crate::stats::record(
            &app,
            &item.package_id,
            user_id.as_deref(),
            crate::stats::Delta {
                requests: 1,
                hits: i64::from(valid),
                misses: i64::from(!valid),
                last_access: Some(app.now()),
                ..Default::default()
            },
        )
        .await?;
    }
    if !valid {
        drop(lease);
        let write = lock.clone().write_owned().await;
        item = artifact(&app, id).await?;
        let latest = app.snapshot().await;
        if item.upstream_identity != latest.settings.registries[kind].upstream_url {
            return Err(Error(
                StatusCode::GONE,
                "Upstream changed; refresh package metadata".into(),
            ));
        }
        valid = item.valid(&latest, app.now()) && tokio::fs::try_exists(item.path(&app)).await?;
        if !valid {
            if tokio::fs::try_exists(item.path(&app)).await? {
                tokio::fs::remove_file(item.path(&app)).await?;
            }
            sqlx::query("UPDATE artifacts SET downloaded_at=NULL,size=0 WHERE id=?")
                .bind(id)
                .execute(&app.db)
                .await?;
            if let Err(error) = download(&app, &latest, &item, user_id.as_deref()).await {
                crate::stats::record(
                    &app,
                    &item.package_id,
                    user_id.as_deref(),
                    crate::stats::Delta {
                        failures: 1,
                        ..Default::default()
                    },
                )
                .await?;
                return Err(error);
            }
        }
        item = artifact(&app, id).await?;
        drop(write);
        lease = lock.read_owned().await;
        if !tokio::fs::try_exists(item.path(&app)).await? {
            return Err(Error::not_found());
        }
    }
    let modified = httpdate::fmt_http_date(
        std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(item.downloaded_at.unwrap_or(0).max(0) as u64),
    );
    let etag = format!("\"{}-{}\"", item.id, item.downloaded_at.unwrap_or(0));
    let unchanged = matches_etag(&headers, &etag)
        || !headers.contains_key("if-none-match")
            && headers
                .get("if-modified-since")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| httpdate::parse_http_date(s).ok())
                .is_some_and(|date| date >= httpdate::parse_http_date(&modified).unwrap());
    let range = if unchanged
        || headers
            .get("if-range")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v != etag && v != modified)
    {
        None
    } else {
        match byte_range(&headers, item.size as u64) {
            Ok(range) => range,
            Err(error) => {
                let mut result = error.into_response();
                result.headers_mut().insert(
                    "content-range",
                    format!("bytes */{}", item.size).parse().unwrap(),
                );
                return Ok(result);
            }
        }
    };
    let (start, end) = range.unwrap_or((0, (item.size as u64).saturating_sub(1)));
    let length = if item.size == 0 { 0 } else { end - start + 1 };
    let body = if method == Method::HEAD || unchanged {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::open(item.path(&app)).await?;
        file.seek(std::io::SeekFrom::Start(start)).await?;
        let mut delivery = Delivery {
            app: app.clone(),
            package_id: item.package_id.clone(),
            bytes: 0,
            user_id,
            _lease: lease,
        };
        Body::from_stream(ReaderStream::new(file.take(length)).map_ok(move |chunk| {
            delivery.record(chunk.len());
            chunk
        }))
    };
    let mut response = Response::new(body);
    *response.status_mut() = if unchanged {
        StatusCode::NOT_MODIFIED
    } else if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    if !unchanged {
        response
            .headers_mut()
            .insert("content-length", length.into());
    }
    if range.is_some() && !unchanged {
        response.headers_mut().insert(
            "content-range",
            format!("bytes {start}-{end}/{}", item.size)
                .parse()
                .unwrap(),
        );
    }
    response.headers_mut().insert(
        "content-type",
        item.content_type
            .as_deref()
            .unwrap_or("application/octet-stream")
            .parse()
            .unwrap_or_else(|_| "application/octet-stream".parse().unwrap()),
    );
    response
        .headers_mut()
        .insert("accept-ranges", "bytes".parse().unwrap());
    response.headers_mut().insert("etag", etag.parse().unwrap());
    response
        .headers_mut()
        .insert("last-modified", modified.parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-cache".parse().unwrap());
    Ok(response)
}
pub async fn evict(app: &App, id: &str, expired_only: bool) -> Result<bool> {
    let _lock = app.artifact_lock(id).write_owned().await;
    let item = artifact(app, id).await?;
    if expired_only && item.valid(&app.snapshot().await, app.now()) {
        return Ok(false);
    }
    match tokio::fs::remove_file(item.path(app)).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    sqlx::query("UPDATE artifacts SET downloaded_at=NULL,size=0 WHERE id=?")
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(true)
}
pub async fn cleanup(app: &App) -> Result<()> {
    let _maintenance = app.maintenance.read().await;
    for row in sqlx::query("SELECT * FROM artifacts WHERE downloaded_at IS NOT NULL")
        .fetch_all(&app.db)
        .await?
    {
        let item = Artifact::from_row(row)?;
        if !item.valid(&app.snapshot().await, app.now()) {
            evict(app, &item.id, true).await?;
        }
    }
    let lifetime = app
        .runtime
        .read()
        .await
        .settings
        .admin_session_lifetime_seconds;
    sqlx::query("DELETE FROM sessions WHERE created_at<=?")
        .bind(app.now().saturating_sub(lifetime as i64))
        .execute(&app.db)
        .await?;
    let lifetime = app.snapshot().await.settings.user_session_lifetime_seconds;
    sqlx::query("DELETE FROM user_sessions WHERE created_at<=?")
        .bind(app.now().saturating_sub(lifetime as i64))
        .execute(&app.db)
        .await?;
    Ok(())
}
