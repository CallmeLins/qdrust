use anyhow::Result;
use qdrust_server::{
    api,
    config::Config,
    email::{EmailClient, EmailConfig},
    scheduler,
    store::Store,
};
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
    let email = EmailClient::new(EmailConfig::from_env())?;
    let (run_events, _) = api::run_event_channel();
    let (subscription_events, _) = api::subscription_event_channel();
    let settings = api::runtime_settings();
    {
        let mut runtime = settings.write().unwrap();
        runtime.require_email_verification = config.require_email_verification;
        runtime.ga_key = config.ga_key.clone();
        runtime.log_retention_days = config.log_retention_days;
    }
    let config_file = config.config_file.clone();
    spawn_settings_watcher(store.clone(), settings.clone(), config_file);
    // The headless browser session manager is optional and process-wide: it is
    // created once when QDRUST_BROWSER_URL is configured and shared by every
    // run so sessions (a login page, a captcha awaiting a human) survive across
    // separate plugin calls. chromiumoxide runs in-process here.
    let browser = qdrust_plugin_browser::BrowserSessionManager::from_env().map(std::sync::Arc::new);
    if let Some(manager) = &browser {
        tracing::info!(endpoint = %manager.endpoint(), "browser session manager enabled");
    }
    // Server-wide default IANA timezone for cron scheduling of tasks that do
    // not carry their own timezone (QDRUST_DEFAULT_TIMEZONE, default
    // Asia/Shanghai). Validated at Config parse time.
    let default_tz: chrono_tz::Tz = config
        .default_timezone
        .parse()
        .unwrap_or(chrono_tz::Asia::Shanghai);
    tracing::info!(tz = %default_tz, "scheduler default timezone");
    let base_path = config.base_path.clone();
    scheduler::spawn(
        store.clone(),
        client.clone(),
        config.scheduler_interval,
        run_events.clone(),
        email,
        config.log_retention_days,
        config.subscription_sync_interval,
        browser,
        default_tz,
    );
    let app = api::router_with_auth(
        store,
        api::AuthConfig {
            session_ttl: config.session_ttl,
            cookie_secure: config.cookie_secure,
            login_rate_limit_attempts: config.login_rate_limit_attempts,
            login_rate_limit_window: config.login_rate_limit_window,
        },
        run_events,
        subscription_events,
        settings.clone(),
        client,
        qdrust_server::redis_cache::SessionCache::from_env()?,
        &base_path,
    )
    .layer(qdrust_server::ga::InjectGaLayer::new(settings))
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

/// Hot-reload watcher: refreshes runtime settings from the optional config
/// file and the site_settings table without restarting the process.
fn spawn_settings_watcher(
    store: qdrust_server::store::Store,
    settings: std::sync::Arc<std::sync::RwLock<api::RuntimeSettings>>,
    config_file: Option<std::path::PathBuf>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
        let mut last_modified = None;
        loop {
            ticker.tick().await;
            if let Some(path) = &config_file {
                match std::fs::metadata(path) {
                    Ok(metadata) => {
                        let modified = metadata.modified().ok();
                        if modified != last_modified {
                            last_modified = modified;
                            if let Ok(source) = std::fs::read_to_string(path)
                                && let Ok(json) = serde_json::from_str::<serde_json::Value>(&source)
                            {
                                let mut runtime = settings.write().unwrap();
                                if let Some(v) = json
                                    .get("require_email_verification")
                                    .and_then(|v| v.as_bool())
                                {
                                    runtime.require_email_verification = v;
                                }
                                if let Some(v) =
                                    json.get("log_retention_days").and_then(|v| v.as_i64())
                                {
                                    runtime.log_retention_days = v.max(0) as u64;
                                }
                                if let Some(v) = json.get("ga_key").and_then(|v| v.as_str()) {
                                    runtime.ga_key = Some(v.to_string());
                                }
                                info!("reloaded runtime settings from {}", path.display());
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%err, path = %path.display(), "cannot stat config file");
                    }
                }
            }
            // site_settings table overrides (admin-editable at runtime)
            if let Ok(Some(setting)) = store.get_setting("require_email_verification").await
                && let Some(v) = setting.value.as_bool()
            {
                settings.write().unwrap().require_email_verification = v;
            }
            if let Ok(Some(setting)) = store.get_setting("ga_key").await
                && let Some(v) = setting.value.as_str()
            {
                settings.write().unwrap().ga_key = Some(v.to_string());
            }
            if let Ok(Some(setting)) = store.get_setting("logs.retention_days").await
                && let Some(v) = setting.value.as_i64()
            {
                settings.write().unwrap().log_retention_days = v.max(0) as u64;
            }
        }
    });
}
