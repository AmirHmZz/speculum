use crate::{App, error::Result};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Row;

#[derive(Default)]
pub struct Delta {
    pub requests: i64,
    pub hits: i64,
    pub misses: i64,
    pub upstream_downloads: i64,
    pub failures: i64,
    pub upstream_bytes: i64,
    pub client_bytes: i64,
    pub last_access: Option<i64>,
}
pub async fn record(
    app: &App,
    package_id: &str,
    user_id: Option<&str>,
    delta: Delta,
) -> Result<()> {
    let mut tx = app.db.begin().await?;
    sqlx::query("UPDATE packages SET requests=requests+?,hits=hits+?,misses=misses+?,upstream_downloads=upstream_downloads+?,failures=failures+?,upstream_bytes=upstream_bytes+?,client_bytes=client_bytes+?,last_access=COALESCE(MAX(last_access,?),last_access,?) WHERE id=?")
        .bind(delta.requests).bind(delta.hits).bind(delta.misses).bind(delta.upstream_downloads).bind(delta.failures).bind(delta.upstream_bytes).bind(delta.client_bytes).bind(delta.last_access).bind(delta.last_access).bind(package_id).execute(&mut *tx).await?;
    if let Some(user_id) = user_id {
        // INSERT ... SELECT prevents an in-flight delivery from recreating a deleted user.
        sqlx::query("INSERT INTO user_package_stats(user_id,package_id,requests,hits,misses,upstream_downloads,failures,upstream_bytes,client_bytes,last_access) SELECT id,?,?,?,?,?,?,?,?,? FROM users WHERE id=? ON CONFLICT(user_id,package_id) DO UPDATE SET requests=requests+excluded.requests,hits=hits+excluded.hits,misses=misses+excluded.misses,upstream_downloads=upstream_downloads+excluded.upstream_downloads,failures=failures+excluded.failures,upstream_bytes=upstream_bytes+excluded.upstream_bytes,client_bytes=client_bytes+excluded.client_bytes,last_access=COALESCE(MAX(last_access,excluded.last_access),last_access,excluded.last_access)")
            .bind(package_id).bind(delta.requests).bind(delta.hits).bind(delta.misses).bind(delta.upstream_downloads).bind(delta.failures).bind(delta.upstream_bytes).bind(delta.client_bytes).bind(delta.last_access).bind(user_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
#[derive(Default, Serialize)]
pub struct Traffic {
    packages: i64,
    requests: i64,
    hits: i64,
    misses: i64,
    upstream_downloads: i64,
    failures: i64,
    upstream_bytes: i64,
    client_bytes: i64,
    last_access: Option<i64>,
}
impl Traffic {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        Self {
            packages: i64::from(row.get::<i64, _>("requests") > 0),
            requests: row.get("requests"),
            hits: row.get("hits"),
            misses: row.get("misses"),
            upstream_downloads: row.get("upstream_downloads"),
            failures: row.get("failures"),
            upstream_bytes: row.get("upstream_bytes"),
            client_bytes: row.get("client_bytes"),
            last_access: row.get("last_access"),
        }
    }
    fn add(&mut self, other: Self) {
        self.packages += other.packages;
        self.requests += other.requests;
        self.hits += other.hits;
        self.misses += other.misses;
        self.upstream_downloads += other.upstream_downloads;
        self.failures += other.failures;
        self.upstream_bytes += other.upstream_bytes;
        self.client_bytes += other.client_bytes;
        self.last_access = self.last_access.max(other.last_access);
    }
}
pub const USER_SELECT: &str = "SELECT s.*,p.id,p.name,p.registry FROM user_package_stats s JOIN packages p ON p.id=s.package_id";
pub async fn summary(app: &App, user_id: &str) -> Result<Value> {
    let rows = sqlx::query(&format!("{USER_SELECT} WHERE s.user_id=?"))
        .bind(user_id)
        .fetch_all(&app.db)
        .await?;
    let mut total = Traffic::default();
    let mut registries = std::collections::BTreeMap::from(
        crate::config::REGISTRIES.map(|kind| (kind, Traffic::default())),
    );
    for row in &rows {
        total.add(Traffic::from_row(row));
        if let Some(traffic) = registries.get_mut(row.get::<String, _>("registry").as_str()) {
            traffic.add(Traffic::from_row(row));
        }
    }
    Ok(json!({"total":total,"registries":registries}))
}
pub fn package_json(row: &sqlx::sqlite::SqliteRow) -> Value {
    let mut value = serde_json::to_value(Traffic::from_row(row)).unwrap();
    let fields = value.as_object_mut().unwrap();
    fields.remove("packages");
    fields.insert("id".into(), json!(row.get::<String, _>("id")));
    fields.insert("name".into(), json!(row.get::<String, _>("name")));
    fields.insert("registry".into(), json!(row.get::<String, _>("registry")));
    value
}
