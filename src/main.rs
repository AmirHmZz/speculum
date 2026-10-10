use speculum::{App, auth, cache};
use std::{
    io::Read,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "speculum=info,tower_http=info".into()),
        )
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "healthcheck") {
        let port = std::env::var("SPECULUM_LISTEN_PORT").unwrap_or_else(|_| "8080".into());
        reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()?
            .get(format!("http://127.0.0.1:{port}/healthz"))
            .send()
            .await?
            .error_for_status()?;
        return Ok(());
    }
    if args == ["admin", "reset-password"] {
        if !Path::new("data/speculum.sqlite").exists() {
            anyhow::bail!("No application database exists; start Speculum first");
        }
        let app = App::open(Path::new("data")).await?;
        let mut password = String::new();
        std::io::stdin().take(1026).read_to_string(&mut password)?;
        let password = password.trim_end_matches(['\r', '\n']).to_owned();
        let hash = auth::hash_password(password)
            .await
            .map_err(|e| anyhow::anyhow!(e.1))?;
        let mut tx = app.db.begin().await?;
        sqlx::query("UPDATE admin SET password_hash=?,must_change_password=1 WHERE id=1")
            .bind(hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM sessions")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        println!("Admin password reset. Start Speculum again.");
        return Ok(());
    }
    if !args.is_empty() {
        anyhow::bail!("Usage: speculum [healthcheck | admin reset-password]");
    }
    let app = App::open(Path::new("data")).await?;
    let ip: IpAddr = std::env::var("SPECULUM_LISTEN_IP")
        .unwrap_or_else(|_| "0.0.0.0".into())
        .parse()?;
    let port: u16 = std::env::var("SPECULUM_LISTEN_PORT")
        .unwrap_or_else(|_| "8080".into())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(ip, port)).await?;
    tracing::info!(address=%listener.local_addr()?,"Speculum listening");
    let cleanup = tokio::spawn(cleanup_loop(app.clone()));
    axum::serve(
        listener,
        speculum::router(app.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    cleanup.abort();
    let _ = cleanup.await;
    app.deliveries.close();
    app.deliveries.wait().await;
    app.db.close().await;
    Ok(())
}
async fn cleanup_loop(app: Arc<App>) {
    loop {
        if let Err(error) = cache::cleanup(&app).await {
            tracing::error!(message=%error.1,"Cache cleanup failed");
        }
        let interval = app.snapshot().await.settings.cleanup_interval_seconds;
        tokio::select! { _ = tokio::time::sleep(std::time::Duration::from_secs(interval)) => {}, _ = app.config_changed.notified() => {} }
    }
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
