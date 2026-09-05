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
    /// IANA timezone applied to cron scheduling when a task does not set its
    /// own `timezone`. Defaults to `Asia/Shanghai` to match a China-first
    /// deployment. Empty means UTC (the previous hardcoded fallback).
    pub default_timezone: String,
    /// URL sub-path the whole site (API + SPA assets) is served under, e.g.
    /// `/qd` when reverse-proxied at `https://host/qd`. Empty means root `/`
    /// (current behaviour, compatible with subdomain or bare deploys).
    pub base_path: String,
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
            default_timezone: env::var("QDRUST_DEFAULT_TIMEZONE")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_default(),
            base_path: normalize_base_path(&env::var("QDRUST_BASE_PATH").unwrap_or_default()),
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
            default_timezone: {
                // Config file is only a fallback; env already won above.
                let file = get("default_timezone").unwrap_or("");
                if !self.default_timezone.is_empty() {
                    self.default_timezone.clone()
                } else {
                    file.to_string()
                }
            },
            base_path: {
                let file = get("base_path").unwrap_or("");
                if self.base_path.is_empty() {
                    normalize_base_path(file)
                } else {
                    self.base_path.clone()
                }
            },
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
        // Default timezone: an unset env + no config-file value falls back to
        // Asia/Shanghai (matches China-first deployments and the WebUI default).
        let default_timezone = if self.default_timezone.trim().is_empty() {
            "Asia/Shanghai".to_string()
        } else {
            self.default_timezone.trim().to_string()
        };
        default_timezone
            .parse::<chrono_tz::Tz>()
            .context("default_timezone must be a valid IANA timezone name")?;
        let base_path = normalize_base_path(&self.base_path);
        Ok(Self {
            default_timezone,
            base_path,
            ..self
        })
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

/// Normalise a configured base path into the form the router expects: either
/// empty (serve at root `/`) or a single leading-slash path with no trailing
/// slash, e.g. `/qd`. Inputs like `/qd/`, `qd`, or `qd/` are all accepted.
fn normalize_base_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return String::new();
    }
    let mut s = trimmed.trim_end_matches('/').to_string();
    if !s.starts_with('/') {
        s.insert(0, '/');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_path_is_normalized() {
        // Empty / "/" / "."-style inputs mean root (no prefix).
        assert_eq!(normalize_base_path(""), "");
        assert_eq!(normalize_base_path("/"), "");
        assert_eq!(normalize_base_path("   "), "");
        // Leading slash preserved, trailing slash stripped.
        assert_eq!(normalize_base_path("/qd"), "/qd");
        assert_eq!(normalize_base_path("/qd/"), "/qd");
        assert_eq!(normalize_base_path("qd"), "/qd");
        assert_eq!(normalize_base_path("qd/"), "/qd");
        // Deep paths are kept.
        assert_eq!(normalize_base_path("/qd/app/"), "/qd/app");
    }

    #[test]
    fn env_parse_defaults_timezone_and_base_path() {
        // Without any env, from_env yields Asia/Shanghai and empty base_path.
        let cfg = Config {
            bind: "0.0.0.0".parse().unwrap(),
            port: 8923,
            database_url: "sqlite://:memory:".into(),
            database_min_connections: 1,
            database_max_connections: 4,
            scheduler_interval: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30),
            session_ttl: Duration::from_secs(60),
            cookie_secure: false,
            database_acquire_timeout: Duration::from_secs(30),
            database_idle_timeout: Duration::from_secs(600),
            login_rate_limit_attempts: 5,
            login_rate_limit_window: Duration::from_secs(60),
            log_retention_days: 0,
            ga_key: None,
            require_email_verification: false,
            subscription_sync_interval: Duration::from_secs(3600),
            default_timezone: String::new(),
            base_path: String::new(),
            config_file: None,
        }
        .validate()
        .unwrap();
        assert_eq!(cfg.default_timezone, "Asia/Shanghai");
        assert_eq!(cfg.base_path, "");
    }

    #[test]
    fn invalid_default_timezone_is_rejected() {
        let cfg = Config {
            bind: "0.0.0.0".parse().unwrap(),
            port: 8923,
            database_url: "sqlite://:memory:".into(),
            database_min_connections: 1,
            database_max_connections: 4,
            scheduler_interval: Duration::from_secs(15),
            request_timeout: Duration::from_secs(30),
            session_ttl: Duration::from_secs(60),
            cookie_secure: false,
            database_acquire_timeout: Duration::from_secs(30),
            database_idle_timeout: Duration::from_secs(600),
            login_rate_limit_attempts: 5,
            login_rate_limit_window: Duration::from_secs(60),
            log_retention_days: 0,
            ga_key: None,
            require_email_verification: false,
            subscription_sync_interval: Duration::from_secs(3600),
            default_timezone: "Not/A_Zone".into(),
            base_path: String::new(),
            config_file: None,
        }
        .validate();
        assert!(cfg.is_err());
    }
}
