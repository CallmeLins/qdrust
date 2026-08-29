use std::{env, net::IpAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    pub database_url: String,
    pub database_min_connections: u32,
    pub database_max_connections: u32,
    pub scheduler_interval: Duration,
    pub request_timeout: Duration,
    pub session_ttl: Duration,
    pub cookie_secure: bool,
    pub database_acquire_timeout: Duration,
    pub database_idle_timeout: Duration,
    pub login_rate_limit_attempts: u32,
    pub login_rate_limit_window: Duration,
    pub log_retention_days: u64,
    pub ga_key: Option<String>,
    pub require_email_verification: bool,
    pub subscription_sync_interval: Duration,
    pub config_file: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            bind: env::var("BIND")
                .unwrap_or_else(|_| "0.0.0.0".into())
                .parse()
                .context("BIND must be an IP address")?,
            port: parse_env("PORT", 8923)?,
            database_url: env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/qd.db".into()),
            database_min_connections: parse_env("DATABASE_MIN_CONNECTIONS", 1)?,
            database_max_connections: parse_env("DATABASE_MAX_CONNECTIONS", 8)?,
            scheduler_interval: Duration::from_secs(parse_env("SCHEDULER_INTERVAL_SECONDS", 15)?),
            request_timeout: Duration::from_secs(parse_env("REQUEST_TIMEOUT_SECONDS", 30)?),
            session_ttl: Duration::from_secs(parse_env("SESSION_TTL_SECONDS", 604_800)?),
            cookie_secure: parse_env("COOKIE_SECURE", false)?,
            database_acquire_timeout: Duration::from_secs(parse_env(
                "DATABASE_ACQUIRE_TIMEOUT_SECONDS",
                30,
            )?),
            database_idle_timeout: Duration::from_secs(parse_env(
                "DATABASE_IDLE_TIMEOUT_SECONDS",
                600,
            )?),
            login_rate_limit_attempts: parse_env("LOGIN_RATE_LIMIT_ATTEMPTS", 5)?,
            login_rate_limit_window: Duration::from_secs(parse_env(
                "LOGIN_RATE_LIMIT_WINDOW_SECONDS",
                60,
            )?),
            log_retention_days: parse_env("LOG_RETENTION_DAYS", 0)?,
            ga_key: env::var("GA_KEY").ok().filter(|s| !s.is_empty()),
            require_email_verification: parse_env("REQUIRE_EMAIL_VERIFICATION", false)?,
            subscription_sync_interval: Duration::from_secs(parse_env(
                "QDRUST_SUBSCRIPTION_SYNC_INTERVAL_SECONDS",
                3600,
            )?),
            config_file: env::var("QDRUST_CONFIG_FILE")
                .ok()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        })
        .and_then(Config::load_config_file)
        .and_then(Config::validate)
        .and_then(Config::validate)
    }

    /// Apply overrides from an optional JSON config file (local_config equivalent).
    /// Environment variables always win over the file.
    fn load_config_file(self) -> Result<Self> {
        let Some(path) = &self.config_file else {
            return Ok(self);
        };
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        let json: serde_json::Value = serde_json::from_str(&source)
            .with_context(|| format!("config file {} is not valid JSON", path.display()))?;
        let get = |key: &str| json.get(key).and_then(|v| v.as_str());
        let get_i64 = |key: &str| json.get(key).and_then(|v| v.as_i64());
        let get_bool = |key: &str| json.get(key).and_then(|v| v.as_bool());
        let apply = |current: &str, key: &str| -> String {
            if !current.is_empty() {
                return current.to_string();
            }
            get(key).unwrap_or(current).to_string()
        };
        let bind = match get("bind") {
            Some(value) => value
                .parse()
                .with_context(|| "config file BIND must be an IP address")?,
            None => self.bind,
        };
        Ok(Self {
            bind,
            port: get_i64("port").map_or(self.port, |v| u16::try_from(v).unwrap_or(self.port)),
            database_url: apply(&self.database_url, "database_url"),
            scheduler_interval: get_i64("scheduler_interval_seconds")
                .map_or(self.scheduler_interval, |v| {
                    Duration::from_secs(v.max(1) as u64)
                }),
            request_timeout: get_i64("request_timeout_seconds").map_or(self.request_timeout, |v| {
                Duration::from_secs(v.max(1) as u64)
            }),
            session_ttl: self.session_ttl,
            cookie_secure: get_bool("cookie_secure").unwrap_or(self.cookie_secure),
            database_min_connections: self.database_min_connections,
            database_max_connections: self.database_max_connections,
            database_acquire_timeout: self.database_acquire_timeout,
            database_idle_timeout: self.database_idle_timeout,
            login_rate_limit_attempts: self.login_rate_limit_attempts,
            login_rate_limit_window: self.login_rate_limit_window,
            log_retention_days: get_i64("log_retention_days")
                .map_or(self.log_retention_days, |v| v.max(0) as u64),
            ga_key: get("ga_key")
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .or(self.ga_key),
            require_email_verification: get_bool("require_email_verification")
                .unwrap_or(self.require_email_verification),
            subscription_sync_interval: get_i64("subscription_sync_interval_seconds")
                .map_or(self.subscription_sync_interval, |v| {
                    Duration::from_secs(v.max(1) as u64)
                }),
            config_file: self.config_file,
        })
    }

    fn validate(self) -> Result<Self> {
        anyhow::ensure!(
            self.database_min_connections <= self.database_max_connections,
            "DATABASE_MIN_CONNECTIONS cannot exceed DATABASE_MAX_CONNECTIONS"
        );
        anyhow::ensure!(
            self.database_max_connections > 0,
            "database pool cannot be empty"
        );
        anyhow::ensure!(!self.session_ttl.is_zero(), "session TTL cannot be zero");
        anyhow::ensure!(
            !self.database_acquire_timeout.is_zero(),
            "database acquire timeout cannot be zero"
        );
        anyhow::ensure!(
            !self.database_idle_timeout.is_zero(),
            "database idle timeout cannot be zero"
        );
        anyhow::ensure!(
            self.login_rate_limit_attempts > 0,
            "login rate limit must allow attempts"
        );
        anyhow::ensure!(
            !self.login_rate_limit_window.is_zero(),
            "login rate limit window cannot be zero"
        );
        Ok(self)
    }
}

fn parse_env<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    env::var(name).map_or(Ok(default), |value| {
        value.parse().with_context(|| format!("invalid {name}"))
    })
}
