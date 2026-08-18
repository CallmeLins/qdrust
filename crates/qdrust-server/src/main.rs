use anyhow::Result;
use qdrust_server::{api, config::Config, scheduler, store::Store};
use tower_http::trace::TraceLayer;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let config = Config::from_env()?;
    let store = Store::connect_with_timeouts(
        &config.database_url,
        config.database_min_connections,
        config.database_max_connections,
        config.database_acquire_timeout,
        config.database_idle_timeout,
    )
    .await?;
    let client = reqwest::Client::builder()
        .timeout(config.request_timeout)
        .build()?;
    scheduler::spawn(store.clone(), client, config.scheduler_interval);
    let app = api::router_with_auth(
        store,
        api::AuthConfig {
            session_ttl: config.session_ttl,
            cookie_secure: config.cookie_secure,
            login_rate_limit_attempts: config.login_rate_limit_attempts,
            login_rate_limit_window: config.login_rate_limit_window,
        },
    )
    .layer(TraceLayer::new_for_http());
    let address = (config.bind, config.port);
    let listener = tokio::net::TcpListener::bind(address).await?;
    info!(address = %listener.local_addr()?, "qdrust started");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
