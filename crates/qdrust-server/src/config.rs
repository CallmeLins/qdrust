use std::{env, net::IpAddr, time::Duration};

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
        })
        .and_then(Config::validate)
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
