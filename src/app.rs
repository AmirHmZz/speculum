use crate::{
    config::{Settings, clients},
    db::{self, SecretKey},
    error::{Error, Result},
};
use sqlx::{Row, SqlitePool};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Notify, RwLock};

pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}
#[derive(Clone)]
pub struct Runtime {
    pub settings: Settings,
    pub clients: BTreeMap<String, reqwest::Client>,
    pub downloads: Arc<DownloadLimiter>,
}
pub struct DownloadLimiter {
    active: Mutex<usize>,
    limit: std::sync::atomic::AtomicUsize,
    changed: Notify,
}
pub struct DownloadPermit(Arc<DownloadLimiter>);
impl DownloadLimiter {
    fn new(limit: usize) -> Self {
        Self {
            active: Mutex::new(0),
            limit: std::sync::atomic::AtomicUsize::new(limit),
            changed: Notify::new(),
        }
    }
    fn set_limit(&self, limit: usize) {
        self.limit.store(limit, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    pub async fn acquire(self: &Arc<Self>) -> DownloadPermit {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut active = self.active.lock().unwrap();
                if *active < self.limit.load(std::sync::atomic::Ordering::SeqCst) {
                    *active += 1;
                    return DownloadPermit(self.clone());
                }
            }
            notified.await;
        }
    }
}
impl Drop for DownloadPermit {
    fn drop(&mut self) {
        *self.0.active.lock().unwrap() -= 1;
        self.0.changed.notify_waiters();
    }
}
pub struct App {
    pub db: SqlitePool,
    pub dir: PathBuf,
    pub runtime: RwLock<Runtime>,
    pub maintenance: RwLock<()>,
    pub config_guard: tokio::sync::Mutex<()>,
    pub config_changed: Notify,
    pub deliveries: tokio_util::task::TaskTracker,
    pub clock: Arc<dyn Clock>,
    pub secret: SecretKey,
    locks: Mutex<HashMap<String, Weak<RwLock<()>>>>,
    pub login_attempts: Mutex<HashMap<String, (u32, i64)>>,
    _lock: File,
}
impl App {
    pub async fn open(dir: &Path) -> anyhow::Result<Arc<Self>> {
        Self::open_with_clock(dir, Arc::new(SystemClock)).await
    }
    pub async fn open_with_clock(dir: &Path, clock: Arc<dyn Clock>) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("instance.lock"))?;
        lock.try_lock().map_err(|_| {
            anyhow::anyhow!("This data volume is already in use by another Speculum instance")
        })?;
        std::fs::create_dir_all(dir.join("artifacts"))?;
        std::fs::create_dir_all(dir.join("tmp"))?;
        for entry in std::fs::read_dir(dir.join("tmp"))? {
            let path = entry?.path();
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }
        let db = db::open(dir).await?;
        let secret = SecretKey::load(dir)?;
        let saved: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE id=1")
            .fetch_optional(&db)
            .await?;
        let mut settings: Settings = if let Some(value) = saved {
            serde_json::from_slice(&secret.decrypt(&value).map_err(|e| anyhow::anyhow!(e.1))?)?
        } else {
            Settings::default()
        };
        settings.upgrade();
        crate::registries::distro::startup(&db, dir).await?;
        let clients = clients(&settings).map_err(|e| anyhow::anyhow!(e.1))?;
        if sqlx::query("SELECT id FROM admin")
            .fetch_optional(&db)
            .await?
            .is_none()
        {
            let password = crate::auth::random_secret().map_err(|e| anyhow::anyhow!(e.1))?;
            let hash = crate::auth::hash_password(password.clone())
                .await
                .map_err(|e| anyhow::anyhow!(e.1))?;
            sqlx::query("INSERT INTO admin(id,username,password_hash,must_change_password) VALUES(1,'admin',?,1)")
                .bind(hash)
                .execute(&db)
                .await?;
            // The initial credential is deliberately printed once, independently of RUST_LOG.
            println!("SPECULUM_INITIAL_ADMIN_PASSWORD={password}");
        }
        let encrypted = secret
            .encrypt(&serde_json::to_vec(&settings)?)
            .map_err(|e| anyhow::anyhow!(e.1))?;
        sqlx::query("INSERT INTO settings(id,value) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET value=excluded.value")
            .bind(encrypted)
            .execute(&db)
            .await?;
        Ok(Arc::new(Self {
            db,
            dir: dir.to_owned(),
            runtime: RwLock::new(Runtime {
                downloads: Arc::new(DownloadLimiter::new(settings.download_concurrency)),
                settings,
                clients,
            }),
            maintenance: RwLock::new(()),
            config_guard: tokio::sync::Mutex::new(()),
            config_changed: Notify::new(),
            deliveries: tokio_util::task::TaskTracker::new(),
            clock,
            secret,
            locks: Mutex::new(HashMap::new()),
            login_attempts: Mutex::new(HashMap::new()),
            _lock: lock,
        }))
    }
    pub fn now(&self) -> i64 {
        self.clock.now()
    }
    pub async fn snapshot(&self) -> Runtime {
        self.runtime.read().await.clone()
    }
    pub fn artifact_lock(&self, id: &str) -> Arc<RwLock<()>> {
        let mut locks = self.locks.lock().unwrap();
        if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
            return lock;
        }
        if locks.len() > 1024 {
            locks.retain(|_, lock| lock.strong_count() > 0);
        }
        let lock = Arc::new(RwLock::new(()));
        locks.insert(id.to_owned(), Arc::downgrade(&lock));
        lock
    }
    pub async fn save_settings(&self, mut settings: Settings) -> Result<()> {
        let _save = self.config_guard.lock().await;
        let previous = self.snapshot().await;
        for (kind, config) in &mut settings.registries {
            if let Some(proxy) = &mut config.proxy
                && proxy.password.as_deref() == Some("********")
            {
                proxy.password = previous
                    .settings
                    .registries
                    .get(kind)
                    .and_then(|c| c.proxy.as_ref())
                    .and_then(|p| p.password.clone());
            }
        }
        let clients = clients(&settings)?;
        let encrypted = self
            .secret
            .encrypt(&serde_json::to_vec(&settings).map_err(|_| Error::bad("Invalid settings"))?)?;
        let mut runtime = self.runtime.write().await;
        sqlx::query("UPDATE settings SET value=? WHERE id=1")
            .bind(encrypted)
            .execute(&self.db)
            .await?;
        // Existing downloads finish; new downloads observe the new shared limit.
        let downloads = previous.downloads;
        downloads.set_limit(settings.download_concurrency);
        *runtime = Runtime {
            settings,
            clients,
            downloads,
        };
        self.config_changed.notify_one();
        Ok(())
    }
    pub async fn package(&self, registry: &str, name: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT OR IGNORE INTO packages(id,registry,name) VALUES(?,?,?)")
            .bind(&id)
            .bind(registry)
            .bind(name)
            .execute(&self.db)
            .await?;
        Ok(
            sqlx::query("SELECT id FROM packages WHERE registry=? AND name=?")
                .bind(registry)
                .bind(name)
                .fetch_one(&self.db)
                .await?
                .get("id"),
        )
    }
    pub async fn registry_enabled(&self, kind: &str) -> Result<()> {
        if !self
            .runtime
            .read()
            .await
            .settings
            .registries
            .get(kind)
            .is_some_and(|c| c.enabled)
        {
            return Err(Error::not_found());
        }
        Ok(())
    }
}
