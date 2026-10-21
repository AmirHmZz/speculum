use crate::{
    App,
    app::Runtime,
    auth::MirrorIdentity,
    cache::{self, Artifact, ArtifactInput, FileDescriptor},
    error::{Error, Result},
    stats::{self, Delta},
};
use axum::{
    Extension,
    extract::{OriginalUri, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use sha2::{Digest, Sha256, Sha512};
use sqlx::{Row, SqlitePool};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::io::AsyncWriteExt;

#[derive(Clone)]
struct Resource {
    generation: Option<String>,
    etag: Option<String>,
    modified: Option<String>,
    validated_at: Option<i64>,
    body_hash: Option<String>,
    immutable: bool,
}
impl Resource {
    async fn load(app: &App, id: &str) -> Result<Self> {
        let row = sqlx::query("SELECT * FROM distro_resources WHERE artifact_id=?")
            .bind(id)
            .fetch_one(&app.db)
            .await?;
        Ok(Self {
            generation: row.get("generation"),
            etag: row.get("etag"),
            modified: row.get("last_modified"),
            validated_at: row.get("validated_at"),
            body_hash: row.get("body_hash"),
            immutable: row.get("immutable"),
        })
    }
    fn path(&self, app: &App) -> Option<PathBuf> {
        self.generation
            .as_ref()
            .map(|generation| app.dir.join("distro").join(generation))
    }
    async fn complete(&self, app: &App, item: &Artifact) -> Result<bool> {
        match self.path(app) {
            Some(path) if item.downloaded_at.is_some() => Ok(tokio::fs::try_exists(path).await?),
            _ => Ok(false),
        }
    }
    fn fresh(&self, app: &App, runtime: &Runtime, item: &Artifact, force: bool) -> bool {
        !force
            && item.valid(runtime, app.now())
            && (self.immutable
                || self.validated_at.is_some_and(|at| {
                    app.now().saturating_sub(at)
                        < runtime.settings.registries[&item.registry].metadata_freshness_seconds
                            as i64
                }))
    }
}

/// Additive table initialization is handled by schema.sql; reconcile disk generations here.
pub async fn startup(db: &SqlitePool, dir: &Path) -> anyhow::Result<()> {
    let folder = dir.join("distro");
    std::fs::create_dir_all(&folder)?;
    let mut live = HashSet::new();
    for row in sqlx::query(
        "SELECT artifact_id,generation FROM distro_resources WHERE generation IS NOT NULL",
    )
    .fetch_all(db)
    .await?
    {
        let generation: String = row.get("generation");
        if folder.join(&generation).is_file() {
            live.insert(generation);
        } else {
            let id: String = row.get("artifact_id");
            sqlx::query("UPDATE artifacts SET downloaded_at=NULL,size=0 WHERE id=?")
                .bind(&id)
                .execute(db)
                .await?;
            sqlx::query("UPDATE distro_resources SET generation=NULL,etag=NULL,last_modified=NULL,validated_at=NULL,body_hash=NULL WHERE artifact_id=?")
                .bind(id).execute(db).await?;
        }
    }
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && !live.contains(&entry.file_name().to_string_lossy().to_string())
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Called under the shared artifact write lock by manual eviction and retention cleanup.
pub(crate) async fn evicted_path(app: &App, id: &str) -> Result<Option<PathBuf>> {
    let resource = Resource::load(app, id).await?;
    sqlx::query("UPDATE distro_resources SET generation=NULL,etag=NULL,last_modified=NULL,validated_at=NULL,body_hash=NULL WHERE artifact_id=?")
        .bind(id).execute(&app.db).await?;
    Ok(resource.path(app))
}

pub async fn root() -> impl IntoResponse {
    (
        [("cache-control", "private, no-store")],
        "Configure this repository URL in APT or APK. Files are mirrored on demand.\n",
    )
}

fn location(uri: &Uri, runtime: &Runtime) -> Result<(String, String, String)> {
    let (kind, encoded) = uri
        .path()
        .trim_start_matches('/')
        .split_once('/')
        .ok_or_else(Error::not_found)?;
    let path = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .map_err(|_| Error::bad("Invalid repository path"))?;
    if path.is_empty()
        || path.contains(['\\', '\0', '\r', '\n', '%', '?', '#'])
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(Error::bad("Invalid repository path"));
    }
    let base = crate::config::http_url(&runtime.settings.registries[kind].upstream_url)?;
    let mut url = base.clone();
    // Append individual decoded segments so URL syntax cannot escape the configured origin/root.
    url.path_segments_mut()
        .map_err(|_| Error::bad("Invalid upstream URL"))?
        .pop_if_empty()
        .extend(path.split('/'));
    if url.origin() != base.origin()
        || !url
            .path()
            .starts_with(&format!("{}/", base.path().trim_end_matches('/')))
    {
        return Err(Error::bad("Repository path escaped its upstream root"));
    }
    Ok((kind.into(), path.into_owned(), url.to_string()))
}

fn forced(headers: &HeaderMap) -> bool {
    headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|part| {
                matches!(
                    part.trim().to_ascii_lowercase().as_str(),
                    "no-cache" | "no-store" | "max-age=0"
                )
            })
        })
        || headers
            .get("pragma")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("no-cache"))
}

pub async fn resource(
    State(app): State<Arc<App>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    method: Method,
    identity: Option<Extension<MirrorIdentity>>,
) -> Result<Response> {
    let _maintenance = app.maintenance.read().await;
    let runtime = app.snapshot().await;
    let (kind, path, url) = location(&uri, &runtime)?;
    let (immutable, sidecar, checksums) = if kind == "alpine" {
        let (immutable, sidecar) = super::apk::classify(&path);
        (immutable, sidecar, BTreeMap::new())
    } else {
        super::apt::classify(&path)?
    };
    let user_id = identity.map(|id| id.0.0);
    let item = cache::register(
        &app,
        &runtime,
        ArtifactInput {
            registry: &kind,
            package: &path,
            version: "",
            filename: path.rsplit('/').next().unwrap(),
            url: &url,
            checksums,
            sidecar,
        },
    )
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO distro_resources(artifact_id,relative_path,immutable) VALUES(?,?,?)",
    )
    .bind(&item.id)
    .bind(&path)
    .bind(immutable)
    .execute(&app.db)
    .await?;
    let lock = app.artifact_lock(&item.id);
    let lease = lock.clone().read_owned().await;
    let cached = Resource::load(&app, &item.id).await?;
    let complete = cached.complete(&app, &item).await?;
    let fresh = complete && cached.fresh(&app, &runtime, &item, forced(&headers));
    if method == Method::GET {
        stats::record(
            &app,
            &item.package_id,
            user_id.as_deref(),
            Delta {
                requests: 1,
                hits: i64::from(fresh),
                misses: i64::from(!fresh),
                last_access: Some(app.now()),
                ..Default::default()
            },
        )
        .await?;
    }
    let (item, cached, lease) = if fresh {
        (item, cached, lease)
    } else {
        drop(lease);
        let write = lock.write_owned().await;
        let item = cache::artifact(&app, &item.id).await?;
        let cached = Resource::load(&app, &item.id).await?;
        let latest = app.snapshot().await;
        if item.upstream_identity != latest.settings.registries[&kind].upstream_url {
            return Err(Error(
                StatusCode::GONE,
                "Upstream changed; retry the repository request".into(),
            ));
        }
        let complete = cached.complete(&app, &item).await?;
        // APT forces revalidation even for checksum-addressed indexes. A failed
        // revalidation must not make an otherwise valid cached file unavailable.
        let can_fallback = complete && (!cached.immutable || item.valid(&latest, app.now()));
        if !complete || !cached.fresh(&app, &latest, &item, forced(&headers)) {
            if method == Method::HEAD {
                let response = head(&latest, &item).await;
                match response {
                    Ok(response) => return Ok(response),
                    Err(error) if can_fallback && error.0 == StatusCode::SERVICE_UNAVAILABLE => {
                        tracing::warn!(
                            registry = kind,
                            path,
                            cache = "stale",
                            "Serving cached repository headers after upstream failure"
                        );
                    }
                    Err(error) => return Err(error),
                }
            } else {
                let mut fetch = FetchState::default();
                let log = cache::FetchLog::new(
                    &kind,
                    if sidecar { "metadata" } else { "artifact" },
                    Method::GET,
                    &item.url,
                );
                let result = fetch
                    .download(&app, &latest, &item, &cached, user_id.as_deref())
                    .await;
                log.finish(&result, fetch.status, fetch.bytes);
                if let Err(error) = result {
                    stats::record(
                        &app,
                        &item.package_id,
                        user_id.as_deref(),
                        Delta {
                            failures: 1,
                            ..Default::default()
                        },
                    )
                    .await?;
                    if can_fallback && fetch.allow_stale {
                        tracing::warn!(registry=kind, path, cache="stale", error=%error.1, "Serving cached repository file after upstream failure");
                    } else {
                        return Err(error);
                    }
                }
            }
        }
        let item = cache::artifact(&app, &item.id).await?;
        let cached = Resource::load(&app, &item.id).await?;
        (
            item,
            cached,
            tokio::sync::OwnedRwLockWriteGuard::downgrade(write),
        )
    };
    tracing::debug!(
        registry = kind,
        path,
        cache = if fresh { "hit" } else { "refreshed_or_stale" },
        "Cache outcome"
    );
    let modified = cached
        .modified
        .clone()
        .filter(|v| httpdate::parse_http_date(v).is_ok())
        .unwrap_or_else(|| {
            httpdate::fmt_http_date(
                std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(item.downloaded_at.unwrap_or(0).max(0) as u64),
            )
        });
    let descriptor = FileDescriptor {
        path: cached.path(&app).ok_or_else(Error::not_found)?,
        etag: format!("\"{}\"", cached.body_hash.as_deref().unwrap_or("")),
        modified,
    };
    let mut response = cache::cached_response(
        app.clone(),
        item,
        headers,
        method,
        user_id,
        lease,
        descriptor,
    )
    .await?;
    response
        .headers_mut()
        .insert("cache-control", "private, no-cache".parse().unwrap());
    Ok(response)
}

async fn head(runtime: &Runtime, item: &Artifact) -> Result<Response> {
    let _permit = runtime.downloads.acquire().await;
    let log = cache::FetchLog::new(&item.registry, "metadata", Method::HEAD, &item.url);
    let mut status = None;
    let result = async {
        let upstream = cache::upstream(
            runtime,
            &item.registry,
            Method::HEAD,
            &item.url,
            HeaderMap::from_iter([(
                "accept-encoding".parse().unwrap(),
                "identity".parse().unwrap(),
            )]),
            None,
        )
        .await
        .map_err(|error| {
            if error.1 == "Unable to reach upstream" {
                Error(StatusCode::SERVICE_UNAVAILABLE, error.1)
            } else {
                error
            }
        })?;
        status = Some(upstream.status().as_u16());
        if upstream.status().is_server_error() {
            return Err(Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Upstream HEAD failed".into(),
            ));
        }
        let mut response = Response::new(axum::body::Body::empty());
        *response.status_mut() = upstream.status();
        for name in [
            "content-type",
            "content-length",
            "last-modified",
            "etag",
            "accept-ranges",
        ] {
            if let Some(value) = upstream.headers().get(name) {
                response.headers_mut().insert(name, value.clone());
            }
        }
        response
            .headers_mut()
            .insert("cache-control", "private, no-cache".parse().unwrap());
        Ok(response)
    }
    .await;
    log.finish(&result, status, 0);
    result
}

#[derive(Default)]
struct FetchState {
    status: Option<u16>,
    bytes: u64,
    allow_stale: bool,
}
impl FetchState {
    async fn download(
        &mut self,
        app: &App,
        runtime: &Runtime,
        item: &Artifact,
        cached: &Resource,
        user_id: Option<&str>,
    ) -> Result<()> {
        let _permit = runtime.downloads.acquire().await;
        let mut headers = HeaderMap::new();
        headers.insert("accept-encoding", "identity".parse().unwrap());
        if cached.complete(app, item).await? && item.valid(runtime, app.now()) {
            for (name, value) in [
                ("if-none-match", &cached.etag),
                ("if-modified-since", &cached.modified),
            ] {
                if let Some(value) = value
                    .as_ref()
                    .and_then(|v| v.parse::<axum::http::HeaderValue>().ok())
                {
                    headers.insert(name, value);
                }
            }
        }
        let response = cache::upstream(
            runtime,
            &item.registry,
            Method::GET,
            &item.url,
            headers,
            None,
        )
        .await
        .inspect_err(|error| {
            self.allow_stale = error.1 == "Unable to reach upstream";
        })?;
        self.status = Some(response.status().as_u16());
        if response.status() == StatusCode::NOT_MODIFIED {
            if !cached.complete(app, item).await? {
                return Err(Error::upstream(
                    "Unexpected upstream 304 without cached body",
                ));
            }
            sqlx::query("UPDATE distro_resources SET validated_at=? WHERE artifact_id=?")
                .bind(app.now())
                .bind(&item.id)
                .execute(&app.db)
                .await?;
            return Ok(());
        }
        if response.status() != StatusCode::OK {
            self.allow_stale = response.status().is_server_error();
            return Err(if response.status() == StatusCode::NOT_FOUND {
                Error::not_found()
            } else {
                Error::upstream("Upstream repository request failed")
            });
        }
        if response
            .headers()
            .get("content-encoding")
            .is_some_and(|v| v != "identity")
        {
            return Err(Error::upstream("Upstream ignored identity encoding"));
        }
        let expected = response.content_length();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let modified = response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let temporary =
            cache::Temporary(app.dir.join("tmp").join(uuid::Uuid::new_v4().to_string()));
        let mut file = tokio::fs::File::create(&temporary.0).await?;
        let mut sha256 = Sha256::new();
        let mut sha512 = Sha512::new();
        let mut stream = response.bytes_stream();
        stats::record(
            app,
            &item.package_id,
            user_id,
            Delta {
                upstream_downloads: 1,
                ..Default::default()
            },
        )
        .await?;
        let transfer: Result<()> = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| {
                    self.allow_stale = true;
                    Error::upstream("Interrupted repository download")
                })?;
                self.bytes += chunk.len() as u64;
                sha256.update(&chunk);
                sha512.update(&chunk);
                file.write_all(&chunk).await?;
            }
            Ok(())
        }
        .await;
        stats::record(
            app,
            &item.package_id,
            user_id,
            Delta {
                upstream_bytes: self.bytes as i64,
                ..Default::default()
            },
        )
        .await?;
        transfer?;
        if expected.is_some_and(|length| length != self.bytes) {
            return Err(Error::upstream("Incomplete repository download"));
        }
        let body_hash = format!("{:x}", sha256.finalize());
        let sha512 = format!("{:x}", sha512.finalize());
        for (algorithm, expected) in &item.checksums {
            let actual = if algorithm == "sha512" {
                &sha512
            } else {
                &body_hash
            };
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(Error::upstream("Repository checksum mismatch"));
            }
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        let generation = uuid::Uuid::new_v4().to_string();
        let mut promoted = cache::Temporary(app.dir.join("distro").join(&generation));
        tokio::fs::rename(&temporary.0, &promoted.0).await?;
        let mut tx = app.db.begin().await?;
        sqlx::query("UPDATE artifacts SET downloaded_at=?,size=?,content_type=? WHERE id=?")
            .bind(app.now())
            .bind(self.bytes as i64)
            .bind(content_type)
            .bind(&item.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE distro_resources SET generation=?,etag=?,last_modified=?,validated_at=?,body_hash=? WHERE artifact_id=?")
            .bind(generation).bind(etag).bind(modified).bind(app.now()).bind(body_hash).bind(&item.id).execute(&mut *tx).await?;
        // A cancelled commit can still succeed in SQLite's worker. From this point,
        // never remove the new body on cancellation; startup reclaims any orphan.
        // The prior body stays intact until commit is known to have succeeded.
        promoted.disarm();
        tx.commit().await?;
        if let Some(old) = cached.path(app) {
            let _ = tokio::fs::remove_file(old).await;
        }
        Ok(())
    }
}
