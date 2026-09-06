use std::{env, net::IpAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result};

/// Effective authentication mode. Always defaults to `local` so an existing
/// deployment keeps its exact login behaviour after an upgrade; external
/// providers only join once the deployer explicitly enables them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    /// Local username/password only (today's behaviour).
    Local,
    /// Local + whatever external providers are enabled.
    Hybrid,
    /// IdP-only: local login entry points are closed (mechanism/data remain).
    Oidc,
}

impl AuthMode {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            // Empty env means "unset" -> strict backward-compatible default.
            "" | "local" => Ok(Self::Local),
            "hybrid" => Ok(Self::Hybrid),
            "oidc" => Ok(Self::Oidc),
            other => anyhow::bail!("invalid auth_mode: {other:?} (expected local|hybrid|oidc)"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Hybrid => "hybrid",
            Self::Oidc => "oidc",
        }
    }
}

/// Public (safe to expose to the browser) snapshot of the login policy.
/// Deliberately contains no secrets such as OIDC client_secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicAuthConfig {
    pub auth_mode: &'static str,
    /// Whether username/password login entry points are reachable.
    pub local_login_enabled: bool,
    pub oidc_enabled: bool,
    /// Display name shown on the SSO button. Empty unless OIDC is enabled.
    pub oidc_provider_name: String,
    pub header_auth_enabled: bool,
}

/// Deep OIDC provider settings consumed by the authorization-code + PKCE flow
/// (Phase 1). The public `/auth/config` endpoint only ever sees the coarse
/// `oidc_enabled` / `oidc_provider_name` booleans — never anything here.
#[derive(Clone, Debug)]
pub struct OidcConfig {
    /// Discovery base URL, e.g. `https://auth.example.com/application/o/qdrust/`.
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// Explicit redirect_uri override. When empty it is derived at request time
    /// from the request host + base_path so one image serves any sub-path.
    pub redirect_uri: String,
    /// Space-separated scopes; defaults to `openid profile email`.
    pub scopes: String,
    /// Auto-provision a local user on first login.
    pub auto_create_users: bool,
    /// Role for auto-provisioned users that are not in an admin group.
    pub default_role: String,
    /// Comma-separated groups that map to the `admin` role.
    pub admin_groups: Vec<String>,
}

impl Default for OidcConfig {
    fn default() -> Self {
        Self {
            issuer: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            scopes: "openid profile email".into(),
            auto_create_users: true,
            default_role: "user".into(),
            admin_groups: Vec::new(),
        }
    }
}

/// Trusted reverse-proxy header authentication (forward-auth) settings
/// (Phase 4). A reverse proxy that has already authenticated the user injects
/// identity headers on every request; the server only honours them when the
/// request originates from a configured trusted proxy IP. Header auth is OFF
/// unless `header_auth_enabled` is set, keeping strict backward compatibility.
#[derive(Clone, Debug)]
pub struct HeaderAuthConfig {
    /// Header carrying the username/subject (e.g. `Remote-User`).
    pub user_header: String,
    /// Header carrying the email (e.g. `Remote-Email`).
    pub email_header: String,
    /// Header carrying group membership (e.g. `Remote-Groups`).
    pub groups_header: String,
    /// Fixed separator used to split the groups header (default `,`).
    pub groups_separator: String,
    /// Source IPs permitted to set identity headers.
    pub trusted_proxies: Vec<IpAddr>,
    /// When true (default) and no trusted proxies are configured while header
    /// auth is enabled, startup fails rather than silently trusting headers.
    pub trusted_proxy_required: bool,
    /// Auto-provision a local user on first header login (default false).
    pub auto_create_users: bool,
    /// Role for auto-provisioned users not in an admin group.
    pub default_role: String,
    /// Groups that map to the `admin` role.
    pub admin_groups: Vec<String>,
}

impl HeaderAuthConfig {
    /// Build the default config (used by `Default` and when disabled).
    pub fn new() -> Self {
        Self {
            user_header: "Remote-User".into(),
            email_header: "Remote-Email".into(),
            groups_header: "Remote-Groups".into(),
            groups_separator: ",".into(),
            trusted_proxies: Vec::new(),
            trusted_proxy_required: true,
            auto_create_users: false,
            default_role: "user".into(),
            admin_groups: Vec::new(),
        }
    }
}

/// `Default` yields the same sane defaults as [`HeaderAuthConfig::new`] (a
/// derived `Default` would leave `default_role` empty and fail `validate`).
impl Default for HeaderAuthConfig {
    fn default() -> Self {
        Self::new()
    }
}

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
    /// Login policy. Defaults to `local` for strict backward compatibility.
    pub auth_mode: AuthMode,
    /// Independent force-switch for the local username/password entry points
    /// (overrides the `auth_mode` derivation). Lets a deployer keep one local
    /// admin backdoor while otherwise going IdP-only.
    pub local_login_enabled: bool,
    pub oidc_enabled: bool,
    /// Display name for the SSO button / `/auth/config`. Explicit public
    /// config (never inferred from the issuer URL). Empty unless OIDC enabled.
    pub oidc_provider_name: String,
    pub header_auth_enabled: bool,
    /// Deep OIDC provider settings for the code+PKCE flow.
    pub oidc: OidcConfig,
    /// Trusted reverse-proxy header authentication settings (Phase 4).
    pub header: HeaderAuthConfig,
    pub config_file: Option<PathBuf>,
}

impl Config {
    /// Derive the public login-policy snapshot returned by `/api/v1/auth/config`.
    /// No secrets here by construction.
    pub fn public_auth_config(&self) -> PublicAuthConfig {
        let local_login_enabled = match self.auth_mode {
            AuthMode::Oidc => self.local_login_enabled, // false unless forced on
            AuthMode::Local | AuthMode::Hybrid => true,
        };
        PublicAuthConfig {
            auth_mode: self.auth_mode.as_str(),
            local_login_enabled,
            oidc_enabled: self.oidc_enabled,
            oidc_provider_name: self.oidc_provider_name.clone(),
            header_auth_enabled: self.header_auth_enabled,
        }
    }
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
            auth_mode: AuthMode::parse(&env::var("QDRUST_AUTH_MODE").unwrap_or_default())
                .context("QDRUST_AUTH_MODE")?,
            local_login_enabled: env::var("QDRUST_LOCAL_LOGIN_ENABLED")
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            oidc_enabled: env::var("QDRUST_OIDC_ENABLED")
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            oidc_provider_name: env::var("QDRUST_OIDC_PROVIDER_NAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_default(),
            oidc: OidcConfig {
                issuer: env::var("QDRUST_OIDC_ISSUER")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_default(),
                client_id: env::var("QDRUST_OIDC_CLIENT_ID")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_default(),
                client_secret: env::var("QDRUST_OIDC_CLIENT_SECRET")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_default(),
                redirect_uri: env::var("QDRUST_OIDC_REDIRECT_URI")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_default(),
                scopes: env::var("QDRUST_OIDC_SCOPES")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "openid profile email".into()),
                auto_create_users: env::var("QDRUST_OIDC_AUTO_CREATE_USERS")
                    .map(|v| v.trim().eq_ignore_ascii_case("true"))
                    .unwrap_or(true),
                default_role: env::var("QDRUST_OIDC_DEFAULT_ROLE")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "user".into()),
                admin_groups: csv_env("QDRUST_OIDC_ADMIN_GROUPS"),
            },
            header_auth_enabled: env::var("QDRUST_HEADER_AUTH_ENABLED")
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            header: HeaderAuthConfig {
                user_header: env::var("QDRUST_HEADER_USER_HEADER")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "Remote-User".into()),
                email_header: env::var("QDRUST_HEADER_EMAIL_HEADER")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "Remote-Email".into()),
                groups_header: env::var("QDRUST_HEADER_GROUPS_HEADER")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "Remote-Groups".into()),
                groups_separator: env::var("QDRUST_HEADER_GROUPS_SEPARATOR")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| ",".into()),
                trusted_proxies: match env::var("QDRUST_HEADER_TRUSTED_PROXIES") {
                    Ok(raw) => raw
                        .split(',')
                        .map(|s| s.trim().parse::<IpAddr>())
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .context(
                            "QDRUST_HEADER_TRUSTED_PROXIES must be comma-separated IP addresses",
                        )?,
                    Err(_) => Vec::new(),
                },
                trusted_proxy_required: env::var("QDRUST_HEADER_TRUSTED_PROXY_REQUIRED")
                    .map(|v| v.trim().eq_ignore_ascii_case("true"))
                    .unwrap_or(true),
                auto_create_users: env::var("QDRUST_HEADER_AUTO_CREATE_USERS")
                    .map(|v| v.trim().eq_ignore_ascii_case("true"))
                    .unwrap_or(false),
                default_role: env::var("QDRUST_HEADER_DEFAULT_ROLE")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "user".into()),
                admin_groups: csv_env("QDRUST_HEADER_ADMIN_GROUPS"),
            },
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
            auth_mode: {
                // Env (when non-default) wins; otherwise honour the file, else local.
                let file = get("auth_mode").unwrap_or("");
                if self.auth_mode != AuthMode::Local && file.is_empty() {
                    self.auth_mode
                } else if !file.is_empty() {
                    AuthMode::parse(file).context("invalid config-file auth_mode")?
                } else {
                    self.auth_mode
                }
            },
            local_login_enabled: get_bool("local_login_enabled")
                .unwrap_or(self.local_login_enabled),
            oidc_enabled: get_bool("oidc_enabled").unwrap_or(self.oidc_enabled),
            oidc_provider_name: get("oidc_provider_name")
                .map(str::to_string)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| self.oidc_provider_name.clone()),
            oidc: {
                // Config-file values only apply when the env value is still the
                // default/empty, matching the "env wins, file is fallback" rule.
                let oidc = json.get("oidc").cloned().unwrap_or(serde_json::Value::Null);
                let file_get = |key: &str| -> String {
                    oidc.get(key)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                OidcConfig {
                    issuer: if self.oidc.issuer.is_empty() {
                        file_get("issuer")
                    } else {
                        self.oidc.issuer.clone()
                    },
                    client_id: if self.oidc.client_id.is_empty() {
                        file_get("client_id")
                    } else {
                        self.oidc.client_id.clone()
                    },
                    client_secret: if self.oidc.client_secret.is_empty() {
                        file_get("client_secret")
                    } else {
                        self.oidc.client_secret.clone()
                    },
                    redirect_uri: if self.oidc.redirect_uri.is_empty() {
                        file_get("redirect_uri")
                    } else {
                        self.oidc.redirect_uri.clone()
                    },
                    scopes: if self.oidc.scopes.is_empty() {
                        let s = file_get("scopes");
                        if s.is_empty() {
                            "openid profile email".into()
                        } else {
                            s
                        }
                    } else {
                        self.oidc.scopes.clone()
                    },
                    auto_create_users: oidc
                        .get("auto_create_users")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(self.oidc.auto_create_users),
                    default_role: if self.oidc.default_role.is_empty() {
                        let s = file_get("default_role");
                        if s.is_empty() { "user".into() } else { s }
                    } else {
                        self.oidc.default_role.clone()
                    },
                    admin_groups: if self.oidc.admin_groups.is_empty() {
                        oidc.get("admin_groups")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    } else {
                        self.oidc.admin_groups.clone()
                    },
                }
            },
            header_auth_enabled: get_bool("header_auth_enabled")
                .unwrap_or(self.header_auth_enabled),
            header: {
                // Config-file values only apply when the env value is still the
                // default/empty, matching the "env wins, file is fallback" rule.
                let h = json
                    .get("header")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let file_get = |key: &str| -> String {
                    h.get(key)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                let parse_ips = |key: &str| -> Vec<IpAddr> {
                    h.get(key)
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str())
                                .filter_map(|s| s.trim().parse::<IpAddr>().ok())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                };
                HeaderAuthConfig {
                    user_header: if self.header.user_header.is_empty() {
                        file_get("user_header")
                    } else {
                        self.header.user_header.clone()
                    },
                    email_header: if self.header.email_header.is_empty() {
                        file_get("email_header")
                    } else {
                        self.header.email_header.clone()
                    },
                    groups_header: if self.header.groups_header.is_empty() {
                        file_get("groups_header")
                    } else {
                        self.header.groups_header.clone()
                    },
                    groups_separator: if self.header.groups_separator.is_empty() {
                        let s = file_get("groups_separator");
                        if s.is_empty() { ",".into() } else { s }
                    } else {
                        self.header.groups_separator.clone()
                    },
                    trusted_proxies: if self.header.trusted_proxies.is_empty() {
                        parse_ips("trusted_proxies")
                    } else {
                        self.header.trusted_proxies.clone()
                    },
                    trusted_proxy_required: h
                        .get("trusted_proxy_required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(self.header.trusted_proxy_required),
                    auto_create_users: h
                        .get("auto_create_users")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(self.header.auto_create_users),
                    default_role: if self.header.default_role.is_empty() {
                        let s = file_get("default_role");
                        if s.is_empty() { "user".into() } else { s }
                    } else {
                        self.header.default_role.clone()
                    },
                    admin_groups: if self.header.admin_groups.is_empty() {
                        h.get("admin_groups")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    } else {
                        self.header.admin_groups.clone()
                    },
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
        // A mode that leans on external providers must actually have one
        // enabled; otherwise refuse loudly rather than silently degrade.
        let oidc_enabled = self.oidc_enabled;
        if !oidc_enabled && matches!(self.auth_mode, AuthMode::Oidc | AuthMode::Hybrid) {
            anyhow::bail!(
                "auth_mode={} requires an external provider enabled \
                 (set QDRUST_OIDC_ENABLED=true and its config)",
                self.auth_mode.as_str()
            );
        }
        // When OIDC is on, the flow needs at least an issuer and client_id
        // (client_secret is required for our confidential-client + PKCE setup).
        if oidc_enabled {
            anyhow::ensure!(
                !self.oidc.issuer.trim().is_empty(),
                "OIDC enabled but QDRUST_OIDC_ISSUER is not set"
            );
            anyhow::ensure!(
                !self.oidc.client_id.trim().is_empty(),
                "OIDC enabled but QDRUST_OIDC_CLIENT_ID is not set"
            );
            anyhow::ensure!(
                !self.oidc.client_secret.trim().is_empty(),
                "OIDC enabled but QDRUST_OIDC_CLIENT_SECRET is not set"
            );
        }
        anyhow::ensure!(
            matches!(self.oidc.default_role.as_str(), "admin" | "user"),
            "QDRUST_OIDC_DEFAULT_ROLE must be admin or user"
        );
        // Header Auth (Phase 4): a trusted proxy is the *only* thing that makes
        // identity headers trustworthy. Refuse to start if header auth is on but
        // the deployer forgot to configure any trusted proxy — silently accepting
        // client-supplied headers would be a critical auth bypass.
        if self.header_auth_enabled
            && self.header.trusted_proxy_required
            && self.header.trusted_proxies.is_empty()
        {
            anyhow::bail!(
                "header auth is enabled with trusted_proxy_required=true but no trusted \
                 proxies are configured (set QDRUST_HEADER_TRUSTED_PROXIES)"
            );
        }
        anyhow::ensure!(
            matches!(self.header.default_role.as_str(), "admin" | "user"),
            "QDRUST_HEADER_DEFAULT_ROLE must be admin or user"
        );
        Ok(Self {
            default_timezone,
            base_path,
            oidc_enabled,
            ..self
        })
    }
}

/// Split a comma-separated env value into trimmed, non-empty parts.
fn csv_env(name: &str) -> Vec<String> {
    env::var(name)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
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
            auth_mode: AuthMode::Local,
            local_login_enabled: false,
            oidc_enabled: false,
            oidc_provider_name: String::new(),
            header_auth_enabled: false,
            oidc: OidcConfig::default(),
            header: HeaderAuthConfig::default(),
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
            auth_mode: AuthMode::Local,
            local_login_enabled: false,
            oidc_enabled: false,
            oidc_provider_name: String::new(),
            header_auth_enabled: false,
            oidc: OidcConfig::default(),
            header: HeaderAuthConfig::default(),
            config_file: None,
        }
        .validate();
        assert!(cfg.is_err());
    }

    #[test]
    fn auth_mode_empty_unset_env_parses_to_local() {
        assert_eq!(AuthMode::parse("").unwrap(), AuthMode::Local);
        assert_eq!(AuthMode::parse("local").unwrap(), AuthMode::Local);
        assert_eq!(AuthMode::parse("LOCAL").unwrap(), AuthMode::Local);
        assert_eq!(AuthMode::parse("hybrid").unwrap(), AuthMode::Hybrid);
        assert_eq!(AuthMode::parse("oidc").unwrap(), AuthMode::Oidc);
        assert!(AuthMode::parse("bogus").is_err());
    }

    #[test]
    fn auth_mode_defaults_to_local_and_public_config_is_safe() {
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
            auth_mode: AuthMode::Local,
            local_login_enabled: false,
            oidc_enabled: false,
            oidc_provider_name: String::new(),
            header_auth_enabled: false,
            oidc: OidcConfig::default(),
            header: HeaderAuthConfig::default(),
            config_file: None,
        }
        .validate()
        .unwrap();
        let pub_cfg = cfg.public_auth_config();
        assert_eq!(pub_cfg.auth_mode, "local");
        assert!(pub_cfg.local_login_enabled);
        assert!(!pub_cfg.oidc_enabled);
        assert!(!pub_cfg.header_auth_enabled);
        assert_eq!(pub_cfg.oidc_provider_name, "");
    }

    #[test]
    fn oidc_mode_keeps_local_entry_only_when_forced_and_public_config_reflects_provider() {
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
            auth_mode: AuthMode::Oidc,
            local_login_enabled: false,
            oidc_enabled: true,
            oidc_provider_name: "Authentik".to_string(),
            header_auth_enabled: false,
            oidc: OidcConfig {
                issuer: "https://auth.example.com/application/o/qdrust/".into(),
                client_id: "qdrust".into(),
                client_secret: "secret".into(),
                ..OidcConfig::default()
            },
            header: HeaderAuthConfig::default(),
            config_file: None,
        }
        .validate()
        .unwrap();
        let pub_cfg = cfg.public_auth_config();
        assert_eq!(pub_cfg.auth_mode, "oidc");
        assert!(!pub_cfg.local_login_enabled); // closed unless forced
        assert!(pub_cfg.oidc_enabled);
        assert_eq!(pub_cfg.oidc_provider_name, "Authentik");
    }

    #[test]
    fn hybrid_without_provider_is_rejected() {
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
            auth_mode: AuthMode::Hybrid,
            local_login_enabled: false,
            oidc_enabled: false,
            oidc_provider_name: String::new(),
            header_auth_enabled: false,
            oidc: OidcConfig::default(),
            header: HeaderAuthConfig::default(),
            config_file: None,
        }
        .validate();
        assert!(cfg.is_err());
    }

    #[test]
    fn hybrid_force_local_login_and_emergency_oidc_mode() {
        // hybrid + explicit oidc_enabled + force local back on -> both visible
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
            auth_mode: AuthMode::Hybrid,
            local_login_enabled: true,
            oidc_enabled: true,
            oidc_provider_name: "Keycloak".to_string(),
            header_auth_enabled: false,
            oidc: OidcConfig {
                issuer: "https://auth.example.com/realms/qdrust".into(),
                client_id: "qdrust".into(),
                client_secret: "secret".into(),
                ..OidcConfig::default()
            },
            header: HeaderAuthConfig::default(),
            config_file: None,
        }
        .validate()
        .unwrap();
        let pub_cfg = cfg.public_auth_config();
        assert_eq!(pub_cfg.auth_mode, "hybrid");
        assert!(pub_cfg.local_login_enabled);
        assert!(pub_cfg.oidc_enabled);
        assert_eq!(pub_cfg.oidc_provider_name, "Keycloak");
    }

    #[test]
    fn header_auth_requires_trusted_proxy_when_required() {
        let base = |header_auth_enabled: bool,
                    trusted_proxies: Vec<IpAddr>,
                    trusted_proxy_required: bool| Config {
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
            auth_mode: AuthMode::Local,
            local_login_enabled: false,
            oidc_enabled: false,
            oidc_provider_name: String::new(),
            header_auth_enabled,
            oidc: OidcConfig::default(),
            header: HeaderAuthConfig {
                trusted_proxies,
                trusted_proxy_required,
                ..HeaderAuthConfig::default()
            },
            config_file: None,
        };

        // Enabled + required + no trusted proxy -> refuse to start (fail fast
        // rather than silently trusting client headers).
        assert!(base(true, vec![], true).validate().is_err());
        // Enabled + required + a trusted proxy -> OK.
        assert!(
            base(true, vec!["127.0.0.1".parse().unwrap()], true)
                .validate()
                .is_ok()
        );
        // Enabled + NOT required (deployer opted out) -> OK even without a proxy.
        assert!(base(true, vec![], false).validate().is_ok());
        // Disabled header auth is always fine.
        assert!(base(false, vec![], true).validate().is_ok());
    }
}
