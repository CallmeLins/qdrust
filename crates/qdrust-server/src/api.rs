use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        ConnectInfo, FromRef, FromRequest, OriginalUri, Path, Query, Request as AxumRequest, State,
        WebSocketUpgrade, rejection::JsonRejection, ws::Message,
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{COOKIE, HeaderName, SET_COOKIE},
    },
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
    routing::{any, delete, get, get_service},
};
use qdrust_core::executor::CancellationToken;
use qdrust_core::plugin::{
    PLUGIN_API_VERSION, Plugin, PluginManifest as CorePluginManifest, PluginRequest,
    SubprocessPlugin,
};
use qdrust_core::qd_har::{QdHar, QdProgram};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;
use tower_http::services::{ServeDir, ServeFile};

use crate::auth::LoginRateLimiter;
use crate::auth::{hash_password, token_hash, verify_password};
use crate::oidc;
use crate::{
    model::{
        AdminUserUpdate, ApplyLibraryTemplate, AuthCredentials, AuthResponse, AuthenticatedSession,
        BatchCreateNotificationAction, BatchTaskOperation, BatchTaskResult, ChangePassword,
        ClearLogs, CreateNotificationAction, CreateNotificationChannel, CreatePluginManifest,
        CreatePushRequest, CreateTask, CreateTemplate, CreateTemplateSubscription,
        DecidePushRequest, ExternalIdentityClaim, ForgotPassword, ImportLibraryTemplates,
        ImportQdHarTemplate, InvokePlugin, IssuedSession, QdHarValidation, RegisterUser,
        ResetPassword, SetSiteSetting, TemplateSubscription, TemplateTestResult, TemplateTestStep,
        TestTemplate, UpdateNotificationAction, UpdateNotificationChannel, UpdatePluginManifest,
        UpdateQdHarTemplate, UpdateTask, UpdateTemplate, UpdateTemplateSubscription, ValidateQdHar,
        VerifyEmail,
    },
    store::Store,
};
use openidconnect::{AuthorizationCode, Nonce, OAuth2TokenResponse, PkceCodeVerifier};

const SESSION_COOKIE: &str = "qd_session";
const CSRF_COOKIE: &str = "qd_csrf";

/// Runtime-tunable settings that can be updated without a restart.
#[derive(Clone, Debug, Default)]
pub struct RuntimeSettings {
    pub require_email_verification: bool,
    pub ga_key: Option<String>,
    pub log_retention_days: u64,
    /// Admin opt-in from ADR-0008: let template runs reach private, loopback
    /// and link-local targets. Off by default — the executor's SSRF guard
    /// stays on, so a template cannot be used to probe the host's network
    /// (or a cloud metadata endpoint) unless an administrator says so.
    pub allow_private_network: bool,
    /// The other half of ADR-0008: accept certificates that do not validate
    /// (self-signed, expired, wrong host). Needed by targets that are only
    /// ever reached on a trusted LAN and were never given a real certificate.
    /// Off by default; the two relaxations are deliberately separate, because
    /// needing one is no reason to grant the other.
    pub allow_invalid_certificates: bool,
}

pub fn runtime_settings() -> std::sync::Arc<std::sync::RwLock<RuntimeSettings>> {
    std::sync::Arc::new(std::sync::RwLock::new(RuntimeSettings::default()))
}

/// Site-settings key for ADR-0008's private-network opt-in. Named once here so
/// the admin API, the settings watcher, the tests and the docs cannot drift
/// apart on the spelling of a key that changes what a template may reach.
pub const ALLOW_PRIVATE_NETWORK_SETTING: &str = "security.allow_private_network";

/// Site-settings key for ADR-0008's invalid-certificate opt-in, the sibling of
/// the key above — same reasoning, same single place to change the spelling.
pub const ALLOW_INVALID_CERTIFICATES_SETTING: &str = "security.allow_invalid_certificates";

/// Apply one stored `site_settings` row to the runtime snapshot.
///
/// Kept as one table so a key cannot be persisted without also being applied:
/// the settings watcher and `admin_set_setting` both go through here, which is
/// the only place the key-to-field mapping is written down. A key that is
/// stored but missing here would silently do nothing.
pub fn apply_runtime_setting(runtime: &mut RuntimeSettings, key: &str, value: &Value) {
    match key {
        "require_email_verification" => {
            if let Some(v) = value.as_bool() {
                runtime.require_email_verification = v;
            }
        }
        "ga_key" => {
            if let Some(v) = value.as_str() {
                runtime.ga_key = Some(v.to_string());
            }
        }
        "logs.retention_days" => {
            if let Some(v) = value.as_i64() {
                runtime.log_retention_days = v.max(0) as u64;
            }
        }
        // ADR-0008: private-network access for **every** outbound request the
        // server makes, not only template runs — the same client serves a task
        // with a plain URL, a notification channel and a library fetch. Off by
        // default; audited like every other setting (`admin.setting_changed`).
        ALLOW_PRIVATE_NETWORK_SETTING => {
            if let Some(v) = value.as_bool() {
                runtime.allow_private_network = v;
            }
        }
        // ADR-0008: accept unvalidated certificates. A sibling of the key
        // above, not a rider on it — reaching a LAN host and skipping hostname
        // verification are different risks, so they stay separate switches.
        ALLOW_INVALID_CERTIFICATES_SETTING => {
            if let Some(v) = value.as_bool() {
                runtime.allow_invalid_certificates = v;
            }
        }
        _ => {}
    }
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub session_ttl: Duration,
    pub cookie_secure: bool,
    pub login_rate_limit_attempts: u32,
    pub login_rate_limit_window: Duration,
    /// Public login-policy snapshot (no secrets), surfaced by
    /// `GET /api/v1/auth/config` and used to gate local entry points.
    pub public: crate::config::PublicAuthConfig,
    /// Public deployment metadata (release version + the WebUI language a
    /// first-time visitor gets), surfaced by `GET /api/v1/meta`. Read by the
    /// WebUI *before* it mounts, so it cannot sit behind a session: the login
    /// page itself has to know what language to speak.
    pub meta: crate::config::PublicMeta,
    /// Deep OIDC provider settings for the authorization-code + PKCE flow.
    pub oidc: crate::config::OidcConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(604_800),
            cookie_secure: false,
            login_rate_limit_attempts: 5,
            login_rate_limit_window: Duration::from_secs(60),
            public: crate::config::PublicAuthConfig {
                auth_mode: "local",
                local_login_enabled: true,
                oidc_enabled: false,
                oidc_provider_name: String::new(),
                header_auth_enabled: false,
                oidc_logout_url: String::new(),
                oidc_post_logout_redirect_uri: String::new(),
            },
            meta: crate::config::PublicMeta::default(),
            oidc: crate::config::OidcConfig::default(),
        }
    }
}

impl AuthConfig {
    /// Whether local username/password entry points may be used. True for
    /// local/hybrid; only closed when oidc-mode unless force-switched on.
    pub fn local_login_enabled(&self) -> bool {
        self.public.local_login_enabled
    }
}

#[derive(Clone)]
struct AppState {
    store: Store,
    auth: AuthConfig,
    login_limiter: LoginRateLimiter,
    run_events: broadcast::Sender<Value>,
    settings: std::sync::Arc<std::sync::RwLock<RuntimeSettings>>,
    /// The only way this crate makes an outbound request. See
    /// [`crate::outbound`] for why it is a type rather than a shared client.
    outbound: crate::outbound::OutboundHttp,
    session_cache: crate::redis_cache::SessionCache,
    /// Catalogues shared between requests, so the aggregate listing does not
    /// re-read every source on each visit. Owned by the router rather than a
    /// process-wide global, which also keeps the tests below from sharing state.
    catalogue_cache: crate::library::CatalogueCache,
    /// The URL sub-path the site is served under (e.g. "/qd" or ""). Threaded
    /// into handlers so OIDC redirect URIs can be derived consistently.
    base_path: String,
    /// Trusted reverse-proxy header auth config. `None` (the default) means
    /// header auth is disabled and the middleware is a no-op pass-through.
    header_auth: Option<std::sync::Arc<crate::config::HeaderAuthConfig>>,
}

impl FromRef<AppState> for Store {
    fn from_ref(state: &AppState) -> Self {
        state.store.clone()
    }
}

/**
 * A run lifecycle event published to all connected WebSocket clients.
 */
#[derive(Clone, Debug, Serialize)]
pub struct RunEvent {
    pub run_id: i64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub status: Option<String>,
    pub step: Option<Value>,
    pub error: Option<String>,
}

impl From<RunEvent> for Value {
    fn from(event: RunEvent) -> Self {
        serde_json::to_value(event).unwrap_or(Value::Null)
    }
}

pub type RunEventSender = broadcast::Sender<Value>;

pub fn run_event_channel() -> (RunEventSender, broadcast::Receiver<Value>) {
    broadcast::channel(512)
}

pub fn router(store: Store) -> Router {
    let (run_events, _) = run_event_channel();
    router_with_auth(
        store,
        AuthConfig::default(),
        run_events,
        runtime_settings(),
        crate::outbound::OutboundHttp::standalone(),
        crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
        "",
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn router_with_auth(
    store: Store,
    auth: AuthConfig,
    run_events: RunEventSender,
    settings: std::sync::Arc<std::sync::RwLock<RuntimeSettings>>,
    outbound: crate::outbound::OutboundHttp,
    session_cache: crate::redis_cache::SessionCache,
    base_path: &str,
    header_auth: Option<std::sync::Arc<crate::config::HeaderAuthConfig>>,
) -> Router {
    let login_limiter =
        LoginRateLimiter::new(auth.login_rate_limit_attempts, auth.login_rate_limit_window)
            .expect("login rate limit configuration must be valid");
    let inner = Router::new()
        .route("/api/v1/openapi.json", get(openapi))
        .route("/api/v1/meta", get(app_meta))
        .route("/api/v1/auth/config", get(auth_config))
        .route("/api/v1/auth/bootstrap", axum::routing::post(bootstrap))
        .route("/api/v1/auth/register", axum::routing::post(register))
        .route("/api/v1/auth/login", axum::routing::post(login))
        .route("/api/v1/auth/session", get(current_session))
        .route("/api/v1/auth/logout", axum::routing::post(logout))
        .route("/api/v1/auth/oidc/start", get(oidc_login_start))
        .route("/api/v1/auth/oidc/callback", get(oidc_login_callback))
        .route(
            "/api/v1/auth/password",
            axum::routing::post(change_password),
        )
        .route(
            "/api/v1/auth/forgot-password",
            axum::routing::post(forgot_password),
        )
        .route(
            "/api/v1/auth/reset-password",
            axum::routing::post(reset_password),
        )
        .route("/api/v1/tasks", get(list_tasks).post(create_task))
        .route("/api/v1/tasks/batch", axum::routing::post(batch_tasks))
        .route("/api/v1/task-groups", get(list_task_groups))
        .route(
            "/api/v1/tasks/{id}",
            get(get_task).put(update_task).delete(delete_task),
        )
        .route(
            "/api/v1/tasks/{id}/runs",
            get(list_task_runs).delete(delete_task_runs),
        )
        .route("/api/v1/tasks/{id}/run", axum::routing::post(run_task))
        .route("/api/v1/runs/{id}/cancel", axum::routing::post(cancel_run))
        .route("/api/v1/runs/{id}/steps", get(list_run_steps))
        .route("/api/v1/runs/{id}/steps/live", get(run_steps_websocket))
        .route("/api/v1/runs/{id}", delete(delete_run))
        .route("/api/v1/runs", get(list_runs).delete(delete_all_runs))
        .route(
            "/api/v1/auth/verify-email",
            axum::routing::post(verify_email),
        )
        .route(
            "/api/v1/auth/resend-verification",
            axum::routing::post(resend_verification),
        )
        .route(
            "/api/v1/auth/csrf/rotate",
            axum::routing::post(rotate_csrf_token),
        )
        .route(
            "/api/v1/subscriptions",
            get(list_subscriptions).post(create_subscription),
        )
        .route(
            "/api/v1/subscriptions/{id}",
            get(get_subscription)
                .put(update_subscription)
                .delete(delete_subscription),
        )
        .route(
            "/api/v1/subscriptions/{id}/library",
            get(browse_subscription_library),
        )
        .route(
            "/api/v1/subscriptions/{id}/library/preview",
            get(preview_subscription_library),
        )
        .route(
            "/api/v1/subscriptions/{id}/library/apply",
            axum::routing::post(apply_subscription_library),
        )
        .route("/api/v1/library", get(library_overview))
        .route(
            "/api/v1/subscriptions/{id}/import",
            axum::routing::post(import_subscription_library),
        )
        .route(
            "/api/v1/push-requests",
            get(list_my_push_requests).post(create_push_request),
        )
        .route("/api/v1/admin/push-requests", get(list_admin_push_requests))
        .route(
            "/api/v1/admin/push-requests/{id}/decision",
            axum::routing::post(decide_push_request),
        )
        .route("/api/v1/admin/backup", get(admin_backup))
        .route("/api/v1/admin/restore", axum::routing::post(admin_restore))
        .route("/api/v1/admin/users", get(admin_list_users))
        .route(
            "/api/v1/admin/users/{id}",
            axum::routing::patch(admin_update_user).delete(admin_delete_user),
        )
        .route("/api/v1/admin/settings", get(admin_list_settings))
        .route(
            "/api/v1/admin/settings/{key}",
            get(admin_get_setting).put(admin_set_setting),
        )
        .route(
            "/api/v1/admin/logs",
            axum::routing::delete(admin_clear_logs),
        )
        .route(
            "/api/v1/notification-channels",
            get(list_notification_channels).post(create_notification_channel),
        )
        .route(
            "/api/v1/notification-channels/{id}",
            get(get_notification_channel)
                .put(update_notification_channel)
                .delete(delete_notification_channel),
        )
        .route(
            "/api/v1/notification-channels/{id}/test",
            axum::routing::post(test_notification_channel),
        )
        .route(
            "/api/v1/tasks/{id}/notification-actions",
            get(list_notification_actions).post(create_notification_action),
        )
        .route(
            "/api/v1/notification-actions",
            axum::routing::get(list_all_notification_actions),
        )
        .route(
            "/api/v1/notification-actions/batch",
            axum::routing::post(batch_create_notification_actions),
        )
        .route(
            "/api/v1/notification-actions/{id}",
            axum::routing::put(update_notification_action).delete(delete_notification_action),
        )
        .route("/api/v1/plugins", get(list_plugins).post(create_plugin))
        .route(
            "/api/v1/plugins/{id}",
            get(get_plugin).put(update_plugin).delete(delete_plugin),
        )
        .route(
            "/api/v1/plugins/{id}/invoke",
            axum::routing::post(invoke_plugin),
        )
        .route("/api/v1/public-templates", get(list_public_templates))
        .route(
            "/api/v1/templates/{id}/publish",
            axum::routing::post(publish_template).delete(unpublish_template),
        )
        .route(
            "/api/v1/templates/{id}/qd-har",
            axum::routing::put(update_qd_har),
        )
        .route(
            "/api/v1/templates/{id}/test",
            axum::routing::post(test_template),
        )
        .route(
            "/api/v1/public-templates/{id}/copy",
            axum::routing::post(copy_public_template),
        )
        .route(
            "/api/v1/templates",
            get(list_templates).post(create_template),
        )
        .route(
            "/api/v1/templates/import-qd-har",
            axum::routing::post(import_qd_har),
        )
        .route(
            "/api/v1/templates/validate-qd-har",
            axum::routing::post(validate_qd_har),
        )
        .route(
            "/api/v1/templates/{id}",
            get(get_template)
                .put(update_template)
                .delete(delete_template),
        )
        .route("/api/{*path}", any(api_not_found))
        // SPA serving. Serve index.html at the router root and fall back to it
        // for any path that is not a real file under dist (deep links, email
        // verify/reset URLs). `.fallback` (NOT `.not_found_service`) is
        // essential: the latter forces every miss to HTTP 404, whereas the SPA
        // must serve index.html with 200 so the browser renders it.
        .route("/", get_service(ServeFile::new("webui/dist/index.html")))
        .fallback_service(
            ServeDir::new("webui/dist").fallback(ServeFile::new("webui/dist/index.html")),
        );

    let state = AppState {
        store,
        auth,
        login_limiter,
        run_events,
        settings,
        outbound,
        session_cache,
        catalogue_cache: crate::library::CatalogueCache::new(),
        base_path: base_path.to_string(),
        header_auth,
    };

    // Header authentication middleware (Phase 4). It runs for every API request
    // (and the SPA fallback) so a trusted reverse proxy can establish a qdrust
    // session from injected identity headers. Auth-management endpoints opt out
    // (see `header_auth_middleware`) so they keep their own session/CSRF flows.
    let inner = inner.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        header_auth_middleware,
    ));

    // Keep liveness/readiness probes at the root (external health checks and
    // the Docker HEALTHCHECK hit `/health`/`/ready` regardless of sub-path).
    let mut root = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready));
    if base_path.trim().is_empty() {
        // Bare-root deployment: `inner` already serves API + SPA (with its own
        // fallback). Nesting would duplicate the fallback, so use it directly.
        root = root.merge(inner);
    } else {
        let nested = Router::new().nest(base_path.trim_end_matches('/'), inner);
        // Outer fallback serves the SPA index for anything still unmatched. This
        // is what catches the trailing-slash nested root `<base>/` (e.g. `/qd/`),
        // which axum's `Router::nest` does not match itself, plus stray paths.
        // `/health`/`/ready` and the nested `/qd/...` tree are matched first.
        root = root
            .merge(nested)
            .fallback_service(ServeFile::new("webui/dist/index.html"));
    }
    root.with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ok","service":"qdrust"}))
}
async fn openapi() -> Json<Value> {
    Json(
        serde_json::from_str(include_str!("../../../docs/openapi-v1.json"))
            .expect("embedded OpenAPI document must be valid JSON"),
    )
}
async fn ready(State(store): State<Store>) -> Result<Json<Value>, ApiError> {
    store.ready().await?;
    Ok(Json(json!({"status":"ready","database":"ok"})))
}
async fn api_not_found() -> ApiError {
    ApiError::NotFound("api_endpoint_not_found", "API endpoint not found")
}

/// Public login-policy endpoint consumed by the WebUI to decide what to render
/// on the login page (local form, SSO button, or auto-redirect). Returns only
/// non-secret values — never client_secret/issuer internals.
async fn auth_config(State(state): State<AppState>) -> Json<Value> {
    let public = &state.auth.public;
    Json(json!({
        "auth_mode": public.auth_mode,
        "local_login_enabled": public.local_login_enabled,
        "oidc_enabled": public.oidc_enabled,
        "oidc_provider_name": public.oidc_provider_name,
        "header_auth_enabled": public.header_auth_enabled,
        "oidc_logout_url": public.oidc_logout_url,
        "oidc_post_logout_redirect_uri": public.oidc_post_logout_redirect_uri,
    }))
}

/// `GET /api/v1/meta`
///
/// Public deployment metadata: the release this server was built from, and the
/// language a first-time visitor should get. The WebUI reads it before it
/// mounts, which is why it carries no secrets and needs no session — an
/// `en-US` deployment would otherwise flash a Chinese frame on every load.
async fn app_meta(State(state): State<AppState>) -> Json<Value> {
    let meta = &state.auth.meta;
    Json(json!({
        "version": meta.version,
        "default_locale": meta.default_locale,
    }))
}

/// `GET /api/v1/auth/oidc/start`
///
/// Kicks off the OIDC Authorization Code + PKCE flow: discovers the provider,
/// builds the authorization URL with a deterministic PKCE verifier/nonce derived
/// from a freshly generated `state`, persists a single-use DB row keyed on
/// `sha256(state)`, and redirects the browser to the IdP. A short-lived
/// `SameSite=Lax` cookie carrying the raw `state` is set so the callback can
/// cross-check it.
async fn oidc_login_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, ApiError> {
    let oidc = &state.auth.oidc;
    if oidc.issuer.trim().is_empty() {
        return Err(ApiError::NotFound(
            "oidc_not_configured",
            "OIDC login is not configured",
        ));
    }

    let redirect_uri = oidc::effective_redirect_uri(oidc, &state.base_path, &headers, &uri);
    let built = oidc::build_client(oidc, &redirect_uri).await?;
    let client = &built.client;

    let raw_state = oidc::generate_state();
    let verifier = oidc::derive_pkce_verifier(&raw_state, &oidc.client_secret);
    let nonce = oidc::derive_nonce(&raw_state, &oidc.client_secret);
    let state_h = oidc::state_hash(&raw_state);
    let nonce_h = oidc::nonce_hash(&nonce);
    let verifier_h = oidc::verifier_hash(&verifier);
    let scopes = oidc::parse_scopes(&oidc.scopes);

    let authorize_url = oidc::build_authorize_url(client, &raw_state, &nonce, &verifier, &scopes);

    let expires_at = chrono::Utc::now().timestamp() + oidc::OIDC_STATE_TTL_SECS;
    state
        .store
        .insert_oidc_login_state(&state_h, &nonce_h, &verifier_h, &redirect_uri, expires_at)
        .await?;

    let mut response = Redirect::to(&authorize_url).into_response();
    let secure_suffix = if state.auth.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let cookie_value = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        oidc::OIDC_STATE_COOKIE,
        raw_state,
        oidc::OIDC_STATE_TTL_SECS,
        secure_suffix
    );
    if let Ok(value) = HeaderValue::from_str(&cookie_value) {
        response.headers_mut().append(SET_COOKIE, value);
    }
    Ok(response)
}

/// `GET /api/v1/auth/oidc/callback`
///
/// The IdP redirects back here with `code`/`state`. Validates the single-use
/// state, recomputes and verifies the PKCE verifier + nonce, exchanges the code,
/// verifies the ID token (issuer/audience/nonce/exp/signature), resolves or
/// provisions the local user, then issues the normal qdrust session as a
/// top-level redirect back to the app. All failure modes redirect to a
/// failure page without ever creating a session.
async fn oidc_login_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let oidc = &state.auth.oidc;
    if oidc.issuer.trim().is_empty() {
        return Err(ApiError::NotFound(
            "oidc_not_configured",
            "OIDC login is not configured",
        ));
    }

    // The IdP reported an error (denied, access_denied, ...): fail without a session.
    if let Some(err) = params.get("error") {
        tracing::warn!(error = %err, "oidc callback received provider error");
        return Ok(oidc_callback_redirect(&state, Some("oidc_provider_error")));
    }

    let code = match params.get("code") {
        Some(c) => c,
        None => return Ok(oidc_callback_redirect(&state, Some("oidc_missing_code"))),
    };
    let state_param = match params.get("state") {
        Some(s) => s,
        None => return Ok(oidc_callback_redirect(&state, Some("oidc_missing_state"))),
    };

    // Cross-check the browser state cookie against the query param (defense in depth).
    match cookie(&headers, oidc::OIDC_STATE_COOKIE) {
        Some(cookie_state) if cookie_state == *state_param => {}
        Some(_) => return Ok(oidc_callback_redirect(&state, Some("oidc_state_mismatch"))),
        None => {
            return Ok(oidc_callback_redirect(
                &state,
                Some("oidc_state_cookie_missing"),
            ));
        }
    }

    let state_h = oidc::state_hash(state_param);
    let stored = match state.store.take_oidc_login_state(&state_h).await? {
        Some(s) => s,
        None => return Ok(oidc_callback_redirect(&state, Some("oidc_state_invalid"))),
    };

    let verifier = oidc::derive_pkce_verifier(state_param, &oidc.client_secret);
    let nonce = oidc::derive_nonce(state_param, &oidc.client_secret);
    if oidc::verifier_hash(&verifier) != stored.pkce_verifier_encrypted
        || oidc::nonce_hash(&nonce) != stored.nonce_hash
    {
        return Ok(oidc_callback_redirect(
            &state,
            Some("oidc_integrity_failed"),
        ));
    }

    let redirect_uri = oidc::effective_redirect_uri(oidc, &state.base_path, &headers, &uri);
    if redirect_uri != stored.redirect_uri {
        return Ok(oidc_callback_redirect(
            &state,
            Some("oidc_redirect_mismatch"),
        ));
    }

    let built = oidc::build_client(oidc, &redirect_uri).await?;
    let verifier_obj = PkceCodeVerifier::new(verifier);
    // openidconnect 4.x: a discovery-built client has a possibly-set token
    // endpoint, so exchange_code is fallible (ConfigurationError if the IdP
    // omitted the token endpoint).
    let token_req = built
        .client
        .exchange_code(AuthorizationCode::new(code.clone()))
        .map_err(|e| {
            ApiError::Internal(anyhow::anyhow!(
                "oidc: provider metadata has no token endpoint: {e}"
            ))
        })?
        .set_pkce_verifier(verifier_obj);
    let token = token_req
        .request_async(&built.http)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("oidc token exchange failed: {e}")))?;

    let id_token = token.extra_fields().id_token().ok_or_else(|| {
        ApiError::Internal(anyhow::anyhow!(
            "oidc: identity provider returned no id_token"
        ))
    })?;
    let nonce_obj = Nonce::new(nonce);
    let claims = id_token
        .claims(&built.client.id_token_verifier(), &nonce_obj)
        .map_err(|e| {
            ApiError::Internal(anyhow::anyhow!("oidc id_token verification failed: {e}"))
        })?;

    let subject = claims.subject().as_str().to_string();
    // TEMP DEBUG — log the verified ID-token claims (non-secret profile fields)
    // so a tester can see exactly what the IdP emitted and confirm the username
    // root cause. Enable with RUST_LOG=qdrust_server=debug; remove after diagnosis.
    if tracing::enabled!(tracing::Level::DEBUG)
        && let Some(payload) = oidc::debug_decode_claims(&id_token.to_string())
    {
        tracing::debug!(subject = %subject, claims = %payload, "oidc id_token claims (debug)");
    }

    // Pick a human-friendly handle for the local username. First try the claims
    // carried in the (verified) ID token: `preferred_username` → `nickname` →
    // `name` → email local-part, skipping any opaque id (long hex/uuid/numeric)
    // so an IdP-assigned handle never shadows a readable one.
    let mut username_hint = oidc::username_hint_from_id_token(
        claims.preferred_username().map(|u| u.as_str()),
        &id_token.to_string(),
    );
    let mut email = claims.email().map(|e| e.as_str().to_string());

    // Many IdPs ship an ID token with only `sub` and put the profile claims
    // behind the UserInfo endpoint (authentik minimal mappers, Keycloak default
    // claims, various enterprise IdPs). When the ID token gave us no usable
    // handle, call UserInfo with the access token to recover the real
    // username/email instead of persisting the opaque `sub`.
    if (username_hint.is_none() || email.is_none())
        && let Some(profile) = oidc::fetch_user_info(
            &built.client,
            &built.http,
            token.access_token().secret(),
            &subject,
        )
        .await
    {
        if email.is_none() {
            email = profile.email.clone();
        }
        if username_hint.is_none() {
            username_hint = oidc::username_hint_from_candidates(
                profile.preferred_username.as_deref(),
                profile.nickname.as_deref(),
                profile.name.as_deref(),
                profile.email.as_deref(),
            );
        }
    }

    // Last resort: the IdP exposed no readable profile anywhere (ID token had
    // only `sub`, and UserInfo was absent or equally bare). Persisting the raw
    // `sub` would render a 36-char uuid as the username, so derive a short,
    // stable handle from it (`user-23cc07f8`) instead. `sub` itself is still
    // stored separately and remains the account's stable identity key.
    if username_hint.is_none() {
        username_hint = Some(oidc::username_fallback_from_subject(&subject));
    }

    // Group membership drives group->admin promotion via `oidc.admin_groups`.
    // The ID token was cryptographically verified by `id_token.claims()` above;
    // re-decode its payload (see `oidc::groups_from_id_token`) to read the
    // IdP-specific `groups_claim` (default "groups"). Absent/empty groups make
    // `resolve_external_identity` fall back to `default_role`.
    let groups = oidc::groups_from_id_token(&id_token.to_string(), &oidc.groups_claim);

    let claim = ExternalIdentityClaim {
        provider: "oidc".to_string(),
        issuer: oidc.issuer.clone(),
        subject,
        email,
        username_hint,
        groups,
    };
    let admin_groups: Vec<&str> = oidc.admin_groups.iter().map(|s| s.as_str()).collect();
    let resolution = state
        .store
        .resolve_external_identity(
            &claim,
            oidc.auto_create_users,
            &oidc.default_role,
            &admin_groups,
        )
        .await?;

    match resolution.user {
        Some(user) => {
            let was_created = resolution.created;
            record_external_login_audit(&state.store, &claim, was_created, &user).await?;
            // Reuse the normal session issuance, then convert the JSON response
            // into a top-level redirect carrying the same session cookies.
            let session_response = issue_session_response(&state, user).await?;
            let mut response = oidc_callback_redirect(&state, None);
            for value in session_response.headers().get_all(SET_COOKIE) {
                response.headers_mut().append(SET_COOKIE, value.clone());
            }
            Ok(response)
        }
        None => {
            let reason = resolution
                .refusal
                .clone()
                .unwrap_or_else(|| "oidc_refused".to_string());
            tracing::warn!(reason = %reason, "oidc login refused by identity resolution");
            // Persistent audit trail for refused external logins. There is no
            // local user to pin the mapping to, so actor is left None and the
            // claimed identity is captured in `details` for an admin to review.
            state
                .store
                .record_audit(
                    None,
                    "auth.external_refused",
                    None,
                    None,
                    None,
                    &json!({
                        "reason": reason,
                        "provider": claim.provider,
                        "issuer": claim.issuer,
                        "subject": claim.subject,
                        "email": claim.email,
                    }),
                )
                .await?;
            Ok(oidc_callback_redirect(&state, Some(&reason)))
        }
    }
}

/// Persist an audit trail entry for a *successful* external (OIDC / future
/// header) login. `was_created` distinguishes a brand-new provisioned external
/// user (whose role was pinned from group membership at first login) from an
/// existing user being reused. See docs/design/EXTERNAL_IDP_PLAN.md Phase 2.
pub(crate) async fn record_external_login_audit(
    store: &Store,
    claim: &ExternalIdentityClaim,
    was_created: bool,
    user: &crate::model::User,
) -> anyhow::Result<()> {
    store
        .record_audit(
            Some(user.id),
            if was_created {
                "auth.external_user_created"
            } else {
                "auth.external_login"
            },
            Some("user"),
            Some(user.id),
            None,
            &json!({
                "provider": claim.provider,
                "issuer": claim.issuer,
                "subject": claim.subject,
            }),
        )
        .await
}

/// Build the full-page redirect the browser ends up on after the OIDC callback.
/// `None` error => success landing page (`{base}/`); `Some(code)` => failure page
/// (`{base}/?login_error=<code>`).
fn oidc_callback_redirect(state: &AppState, error: Option<&str>) -> Response {
    let base = state.base_path.trim_end_matches('/');
    let location = match error {
        Some(code) => format!("{base}/?login_error={code}"),
        None => format!("{base}/"),
    };
    let mut response = Redirect::to(&location).into_response();
    // Always clear the short-lived OIDC state cookie on the callback response,
    // whether login succeeded or failed.
    oidc::append_clear_oidc_cookie(response.headers_mut(), state.auth.cookie_secure);
    response
}

async fn bootstrap(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<AuthCredentials>,
) -> Result<Response, ApiError> {
    let password = input.password;
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(anyhow::Error::from)??;
    let user = state
        .store
        .create_first_admin(&input.username, &password_hash)
        .await?
        .ok_or(ApiError::Conflict(
            "bootstrap_already_completed",
            "Initial administrator already exists".into(),
        ))?;
    state
        .store
        .record_audit(
            Some(user.id),
            "auth.bootstrap",
            Some("user"),
            Some(user.id),
            None,
            &json!({}),
        )
        .await?;
    issue_session_response(&state, user).await
}

/// Reject username/password entry points when the deployment has closed local
/// login (e.g. oidc-mode). Does not affect existing sessions or password/token
/// flows that act on an already-known user.
fn ensure_local_login_allowed(state: &AppState) -> Result<(), ApiError> {
    if state.auth.local_login_enabled() {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "local_login_disabled",
            "Local username/password login is disabled on this deployment",
        ))
    }
}

async fn login(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<AuthCredentials>,
) -> Result<Response, ApiError> {
    ensure_local_login_allowed(&state)?;
    let rate_key = input.username.trim().to_ascii_lowercase();
    if !state.login_limiter.allowed(&rate_key).await {
        return Err(ApiError::TooManyRequests(
            "login_rate_limited",
            "Too many login attempts",
        ));
    }
    let credentials = state.store.credentials_by_username(&input.username).await?;
    let encoded_hash = credentials
        .as_ref()
        .map(|credentials| credentials.password_hash.clone())
        .unwrap_or_else(|| {
            hash_password("qdrust dummy password value")
                .expect("the fixed dummy password meets policy")
        });
    let password = input.password;
    let valid = tokio::task::spawn_blocking(move || verify_password(&password, &encoded_hash))
        .await
        .map_err(anyhow::Error::from)?;
    let credentials = credentials.filter(|credentials| valid && !credentials.user.disabled);
    if let Some(credentials) = credentials.as_ref() {
        let require_verification = state
            .settings
            .read()
            .map(|s| s.require_email_verification)
            .unwrap_or(false);
        if require_verification && !credentials.user.email_verified {
            return Err(ApiError::Forbidden(
                "email_not_verified",
                "Email address is not verified",
            ));
        }
    }
    if credentials.is_none() {
        state.login_limiter.record_failure(&rate_key).await;
        state
            .store
            .record_audit(
                None,
                "auth.login_failed",
                Some("user"),
                None,
                None,
                &json!({"username": rate_key}),
            )
            .await?;
    } else {
        state.login_limiter.record_success(&rate_key).await;
    }
    let user = credentials
        .map(|credentials| credentials.user)
        .ok_or(ApiError::Unauthorized(
            "invalid_credentials",
            "Invalid username or password",
        ))?;
    state
        .store
        .record_audit(
            Some(user.id),
            "auth.login",
            Some("user"),
            Some(user.id),
            None,
            &json!({}),
        )
        .await?;
    issue_session_response(&state, user).await
}

async fn current_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    Ok(Json(
        json!({"user": session.user, "expires_at": session.expires_at}),
    ))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let (session_token, session) = require_session(&state, &headers).await?;
    require_csrf(&headers, &session)?;
    state.store.revoke_session(&session_token).await?;
    state
        .store
        .record_audit(
            Some(session.user.id),
            "auth.logout",
            Some("session"),
            None,
            None,
            &json!({}),
        )
        .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    append_clear_cookies(response.headers_mut(), state.auth.cookie_secure);
    Ok(response)
}

async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<ChangePassword>,
) -> Result<Response, ApiError> {
    let (_session_token, session) = require_session(&state, &headers).await?;
    require_csrf(&headers, &session)?;
    let credentials = state
        .store
        .credentials_by_username(&session.user.username)
        .await?
        .ok_or(ApiError::Unauthorized(
            "authentication_required",
            "Authentication required",
        ))?;
    let current_password = input.current_password;
    let encoded_hash = credentials.password_hash;
    let valid =
        tokio::task::spawn_blocking(move || verify_password(&current_password, &encoded_hash))
            .await
            .map_err(anyhow::Error::from)?;
    if !valid {
        return Err(ApiError::Unauthorized(
            "invalid_credentials",
            "Current password is invalid",
        ));
    }
    let new_password = input.new_password;
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&new_password))
        .await
        .map_err(anyhow::Error::from)??;
    if !state
        .store
        .change_password(session.user.id, &password_hash)
        .await?
    {
        return Err(ApiError::Unauthorized(
            "authentication_required",
            "Authentication required",
        ));
    }
    state
        .store
        .record_audit(
            Some(session.user.id),
            "auth.password_changed",
            Some("user"),
            Some(session.user.id),
            None,
            &json!({}),
        )
        .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    append_clear_cookies(response.headers_mut(), state.auth.cookie_secure);
    Ok(response)
}

async fn issue_session_response(
    state: &AppState,
    user: crate::model::User,
) -> Result<Response, ApiError> {
    let issued = state
        .store
        .create_session(user.id, state.auth.session_ttl)
        .await?;
    let mut response = Json(AuthResponse {
        user,
        expires_at: issued.expires_at,
    })
    .into_response();
    append_session_cookies(response.headers_mut(), &issued, &state.auth)?;
    Ok(response)
}

async fn require_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(String, AuthenticatedSession), ApiError> {
    let token = cookie(headers, SESSION_COOKIE).ok_or(ApiError::Unauthorized(
        "authentication_required",
        "Authentication required",
    ))?;
    let token_hash = crate::auth::token_hash(&token);
    if let Some(session) = state.session_cache.get(&token_hash).await {
        return Ok((token, session));
    }
    let session = state
        .store
        .authenticate_session(&token)
        .await?
        .ok_or(ApiError::Unauthorized(
            "authentication_required",
            "Authentication required",
        ))?;
    state
        .session_cache
        .set(
            &token_hash,
            &session,
            i64::try_from(state.auth.session_ttl.as_secs().min(86_400 * 7)).unwrap_or(86_400 * 7),
        )
        .await;
    Ok((token, session))
}

async fn require_session_from_store(
    store: &Store,
    headers: &HeaderMap,
) -> Result<(String, AuthenticatedSession), ApiError> {
    let token = cookie(headers, SESSION_COOKIE).ok_or(ApiError::Unauthorized(
        "authentication_required",
        "Authentication required",
    ))?;
    let session = store
        .authenticate_session(&token)
        .await?
        .ok_or(ApiError::Unauthorized(
            "authentication_required",
            "Authentication required",
        ))?;
    Ok((token, session))
}

fn require_csrf(headers: &HeaderMap, session: &AuthenticatedSession) -> Result<(), ApiError> {
    let cookie_token = cookie(headers, CSRF_COOKIE).ok_or(ApiError::Forbidden(
        "csrf_validation_failed",
        "CSRF validation failed",
    ))?;
    let header_token = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Forbidden(
            "csrf_validation_failed",
            "CSRF validation failed",
        ))?;
    let same_token = cookie_token
        .as_bytes()
        .ct_eq(header_token.as_bytes())
        .into();
    let expected_hash = token_hash(header_token);
    let valid_hash: bool = expected_hash
        .as_bytes()
        .ct_eq(session.csrf_token_hash.as_bytes())
        .into();
    if same_token && valid_hash {
        Ok(())
    } else {
        Err(ApiError::Forbidden(
            "csrf_validation_failed",
            "CSRF validation failed",
        ))
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

fn append_session_cookies(
    headers: &mut HeaderMap,
    session: &IssuedSession,
    config: &AuthConfig,
) -> Result<(), ApiError> {
    let max_age = config.session_ttl.as_secs();
    let secure = if config.cookie_secure { "; Secure" } else { "" };
    let session_cookie = format!(
        "{SESSION_COOKIE}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}",
        session.session_token
    );
    let csrf_cookie = format!(
        "{CSRF_COOKIE}={}; Path=/; SameSite=Strict; Max-Age={max_age}{secure}",
        session.csrf_token
    );
    headers.append(SET_COOKIE, HeaderValue::from_str(&session_cookie)?);
    headers.append(SET_COOKIE, HeaderValue::from_str(&csrf_cookie)?);
    Ok(())
}

fn append_clear_cookies(headers: &mut HeaderMap, secure: bool) {
    let secure = if secure { "; Secure" } else { "" };
    for cookie in [
        format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}"),
        format!("{CSRF_COOKIE}=; Path=/; SameSite=Strict; Max-Age=0{secure}"),
    ] {
        headers.append(
            SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("static cookie attributes are valid"),
        );
    }
}

/// Axum middleware implementing trusted reverse-proxy header authentication
/// (Phase 4, forward-auth). Returns a `Response` directly; on refusal it
/// short-circuits with 403 and an audit entry. See docs/design/EXTERNAL_IDP_PLAN.md Phase 4.
async fn header_auth_middleware(
    State(state): State<AppState>,
    mut request: AxumRequest,
    next: Next,
) -> Response {
    // Header auth disabled -> transparent pass-through.
    let Some(cfg) = state.header_auth.clone() else {
        return next.run(request).await;
    };
    // Auth-management endpoints own their session/CSRF flows; never auto-login
    // through them (avoids double session creation on /auth/login, CSRF issues
    // on /auth/logout, and clobbering the OIDC callback).
    if request.uri().path().starts_with("/api/v1/auth") {
        return next.run(request).await;
    }

    let headers = request.headers().clone();
    let Some(identity) = crate::header_auth::extract_header_identity(&headers, &cfg) else {
        // No identity headers -> leave normal cookie-based auth to the handlers.
        return next.run(request).await;
    };

    // Source-IP trust comes from `ConnectInfo`, populated by
    // `into_make_service_with_connect_info` at startup. If it is missing (e.g. a
    // direct oneshot test without an injected extension) we treat the source as
    // untrusted rather than risk trusting spoofed headers.
    let trusted = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| crate::header_auth::source_is_trusted(ci.0.ip(), &cfg.trusted_proxies))
        .unwrap_or(false);

    if !trusted {
        // Injection safety: never let client-supplied copies of these headers
        // reach application logic.
        crate::header_auth::strip_identity_headers(&mut request, &cfg);
        if cfg.trusted_proxy_required {
            let _ = record_header_refused(&state, &identity, "untrusted_source").await;
            return ApiError::Forbidden(
                "untrusted_proxy_header",
                "Identity headers from an untrusted source are not accepted",
            )
            .into_response();
        }
        // Not required -> ignore the (stripped) headers and continue normally.
        return next.run(request).await;
    }

    // Trusted source: resolve the external identity against the DB.
    let existing = require_session(&state, &headers).await.ok();
    let claim = ExternalIdentityClaim {
        provider: "header".into(),
        issuer: "header".into(),
        subject: identity.username.clone(),
        email: identity.email.clone(),
        username_hint: Some(identity.username.clone()),
        groups: identity.groups.clone(),
    };
    let admin_groups: Vec<&str> = cfg.admin_groups.iter().map(String::as_str).collect();
    let resolution = match state
        .store
        .resolve_external_identity(
            &claim,
            cfg.auto_create_users,
            &cfg.default_role,
            &admin_groups,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(%e, "header auth identity resolution failed");
            return ApiError::Internal(anyhow::anyhow!("header auth resolution failed"))
                .into_response();
        }
    };

    match resolution.user {
        Some(user) => {
            // Reuse: an existing valid session already belongs to this user.
            if let Some((_, sess)) = &existing
                && sess.user.id == user.id
            {
                return next.run(request).await;
            }
            // Establish (or refresh on identity drift) a session for the user.
            let old_token = existing.as_ref().map(|(token, _)| token.clone());
            match establish_header_session(&state, &user, old_token.as_deref()).await {
                Ok(issued) => {
                    inject_session_cookies(&mut request, &issued, &state.auth);
                    let mut response = next.run(request).await;
                    let _ = append_session_cookies(response.headers_mut(), &issued, &state.auth);
                    let _ = record_external_login_audit(
                        &state.store,
                        &claim,
                        resolution.created,
                        &user,
                    )
                    .await;
                    response
                }
                Err(e) => {
                    tracing::error!(?e, "header auth session creation failed");
                    ApiError::Internal(anyhow::anyhow!("header auth session creation failed"))
                        .into_response()
                }
            }
        }
        None => {
            // Refusal. If we already have a valid session (e.g. a transient
            // header glitch on an established session) keep it rather than
            // locking the user out; otherwise refuse the login.
            if existing.is_some() {
                return next.run(request).await;
            }
            let reason = resolution
                .refusal
                .clone()
                .unwrap_or_else(|| "header_identity_refused".into());
            let _ = record_header_refused(&state, &identity, &reason).await;
            ApiError::Forbidden(
                "header_identity_refused",
                "Header identity could not be resolved to a local user",
            )
            .into_response()
        }
    }
}

/// Mint a fresh qdrust session for a header-resolved user. When a previous
/// session exists for a *different* user (identity drift) the old one is revoked
/// so sessions do not accumulate across drift events.
async fn establish_header_session(
    state: &AppState,
    user: &crate::model::User,
    old_token: Option<&str>,
) -> anyhow::Result<crate::model::IssuedSession> {
    if let Some(old) = old_token {
        let _ = state.store.revoke_session(old).await;
    }
    let issued = state
        .store
        .create_session(user.id, state.auth.session_ttl)
        .await?;
    Ok(issued)
}

/// Inject the just-created session cookies into the *request* so the handler
/// invoked in the same call sees an authenticated session (otherwise the very
/// first request after a header login would 401 before the browser persists the
/// `Set-Cookie`). The caller also sets them on the response for persistence.
fn inject_session_cookies(
    request: &mut AxumRequest,
    issued: &crate::model::IssuedSession,
    _auth: &AuthConfig,
) {
    let session_cookie = format!("{SESSION_COOKIE}={}", issued.session_token);
    let csrf_cookie = format!("{CSRF_COOKIE}={}", issued.csrf_token);
    let combined = format!("{session_cookie}; {csrf_cookie}");
    let merged = match request.headers().get(COOKIE).and_then(|v| v.to_str().ok()) {
        Some(existing) if !existing.is_empty() => format!("{existing}; {combined}"),
        _ => combined,
    };
    if let Ok(value) = HeaderValue::from_str(&merged) {
        request.headers_mut().insert(COOKIE, value);
    }
}

/// Audit a refused header login (untrusted source or unresolved identity).
async fn record_header_refused(
    state: &AppState,
    identity: &crate::header_auth::HeaderIdentity,
    reason: &str,
) -> anyhow::Result<()> {
    state
        .store
        .record_audit(
            None,
            "auth.external_refused",
            None,
            None,
            None,
            &json!({
                "reason": reason,
                "provider": "header",
                "issuer": "header",
                "subject": identity.username,
                "email": identity.email,
            }),
        )
        .await
}
async fn list_tasks(
    State(store): State<Store>,
    headers: HeaderMap,
    // Safe only while every parameter here is read as text: a number would
    // arrive as a string and `as_i64` would find nothing — see `RunsQuery`.
    Query(params): Query<serde_json::Value>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let grp = params.get("grp").and_then(|v| v.as_str());
    Ok(Json(json!(
        store
            .list_for_owner_with_group(session.user.id, grp)
            .await?
    )))
}
async fn create_task(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateTask>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let task = store
        .create_for_owner(session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?;
    Ok((StatusCode::CREATED, Json(json!(task))))
}
async fn get_task(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .get_for_owner(id, session.user.id)
        .await?
        .map(|t| Json(json!(t)))
        .ok_or(ApiError::NotFound("task_not_found", "Task not found"))
}
async fn update_task(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdateTask>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .update_for_owner(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|t| Json(json!(t)))
        .ok_or(ApiError::NotFound("task_not_found", "Task not found"))
}
async fn delete_task(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    if store.delete_for_owner(id, session.user.id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("task_not_found", "Task not found"))
    }
}

async fn list_task_runs(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let Some(runs) = store.list_task_runs_for_owner(id, session.user.id).await? else {
        return Err(ApiError::NotFound("task_not_found", "Task not found"));
    };
    Ok(Json(json!(runs)))
}

/// The aggregated run log's query string.
///
/// Typed, rather than read off a `serde_json::Value`. A query string carries
/// every value as text and `Value` keeps it that way, so `as_i64` found nothing:
/// `limit` fell back to the default 100 rows whatever the reader picked,
/// `before_id` was always absent — which made the cursor a no-op, so "next page"
/// re-served the first one — and `task_id` never filtered. Only `status` worked,
/// because `as_str` is the one reading a text value can satisfy, and that is
/// what kept the others quiet for so long. Typed extraction parses the numbers.
/// `#[serde(default)]` on every field: the WebUI omits a filter it is not using,
/// and a query string is allowed to leave any of them out.
#[derive(Deserialize, Default)]
struct RunsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    task_id: Option<i64>,
    #[serde(default)]
    before_id: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

/// Every run the caller owns, newest first — the aggregated log view.
///
/// Query parameters: `status` (exact match), `task_id`, `limit` (default 100,
/// max 500) and `before_id` (keyset cursor: the `next_cursor` of the previous
/// page). Returns `{items, has_more, next_cursor}` like the template list.
async fn list_runs(
    State(store): State<Store>,
    headers: HeaderMap,
    Query(query): Query<RunsQuery>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let filter = crate::store::RunFilter {
        status: query
            .status
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        task_id: query.task_id,
        before_id: query.before_id,
        limit,
    };
    let mut runs = store
        .search_runs_for_owner(session.user.id, &filter)
        .await?;
    let has_more = runs.len() as i64 > limit;
    if has_more {
        runs.truncate(limit as usize);
    }
    let next_cursor = if has_more {
        runs.last().map(|run| run.id)
    } else {
        None
    };
    Ok(Json(json!({
        "items": runs,
        "has_more": has_more,
        "next_cursor": next_cursor,
    })))
}

async fn run_task(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    if store.get_for_owner(id, session.user.id).await?.is_none() {
        return Err(ApiError::NotFound("task_not_found", "Task not found"));
    }
    let run = store.enqueue_run(id).await?.ok_or(ApiError::Conflict(
        "task_already_running",
        "Task already has an active run".into(),
    ))?;
    Ok((StatusCode::ACCEPTED, Json(json!(run))))
}

async fn cancel_run(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let Some(run) = store.get_run(id).await? else {
        return Err(ApiError::NotFound("run_not_found", "Run not found"));
    };
    if store
        .get_for_owner(run.task_id, session.user.id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound("run_not_found", "Run not found"));
    }
    if store.cancel_run(id).await? {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ApiError::Conflict(
            "run_not_active",
            "Run is not active".into(),
        ))
    }
}

async fn delete_run(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let Some(run) = store.get_run(id).await? else {
        return Err(ApiError::NotFound("run_not_found", "Run not found"));
    };
    if store
        .get_for_owner(run.task_id, session.user.id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound("run_not_found", "Run not found"));
    }
    if store.delete_run(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("run_not_found", "Run not found"))
    }
}

/// Clear the run history: admins wipe every run, users wipe the runs of their
/// own tasks.
async fn delete_all_runs(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let deleted = if session.user.role == "admin" {
        store.delete_all_runs().await?
    } else {
        store.delete_runs_for_owner(session.user.id).await?
    };
    Ok(Json(json!({ "deleted": deleted })))
}

/// Clear all runs of one task. Only the task owner (or an admin) may do this.
async fn delete_task_runs(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    if session.user.role != "admin" && store.get_for_owner(id, session.user.id).await?.is_none() {
        return Err(ApiError::NotFound("task_not_found", "Task not found"));
    }
    let deleted = store.delete_runs_for_task(id).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

async fn list_run_steps(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let Some(steps) = store.list_run_steps_for_owner(id, session.user.id).await? else {
        return Err(ApiError::NotFound("run_not_found", "Run not found"));
    };
    Ok(Json(json!(steps)))
}

async fn list_notification_channels(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok(Json(json!(
        store.list_notification_channels(session.user.id).await?
    )))
}
async fn create_notification_channel(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateNotificationChannel>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    // Single source of truth for the accepted kinds (see push_channels.rs); the
    // store's config validator accepts the very same set, so a channel kind the
    // UI offers can never be rejected here again (that is how `custom_http`
    // once became un-creatable).
    if !crate::push_channels::is_known_channel_kind(&input.kind) {
        return Err(ApiError::unprocessable(anyhow::anyhow!(
            "unsupported notification channel kind: {}",
            input.kind
        )));
    }
    let channel = store
        .create_notification_channel(session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?;
    Ok((StatusCode::CREATED, Json(json!(channel))))
}
async fn get_notification_channel(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .get_notification_channel(id, session.user.id)
        .await?
        .map(|v| Json(json!(v)))
        .ok_or(ApiError::NotFound(
            "notification_channel_not_found",
            "Notification channel not found",
        ))
}
async fn update_notification_channel(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdateNotificationChannel>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .update_notification_channel(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|v| Json(json!(v)))
        .ok_or(ApiError::NotFound(
            "notification_channel_not_found",
            "Notification channel not found",
        ))
}
async fn delete_notification_channel(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    if store
        .delete_notification_channel(id, session.user.id)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(
            "notification_channel_not_found",
            "Notification channel not found",
        ))
    }
}

/// Send a test message through one channel.
///
/// Goes through the same [`crate::delivery::deliver`] the scheduler uses, so a
/// green test means the credentials and the transport work. A delivery failure
/// is reported verbatim in a 422: the whole point is to show the user why their
/// channel is silent.
async fn test_notification_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&state.store, &headers).await?;
    let channel = state
        .store
        .get_notification_channel(id, session.user.id)
        .await?
        .ok_or(ApiError::NotFound(
            "notification_channel_not_found",
            "Notification channel not found",
        ))?;
    let timezone = crate::config::Config::from_env()
        .map(|config| config.default_timezone)
        .ok();
    let vars = crate::delivery::test_vars(timezone.as_deref());
    // Title/body templates live on the task↔channel bindings, not on the
    // channel, so a channel-level test can only exercise the generic message.
    let title = vars.render("[qdrust] Test notification");
    let body = vars.render(
        "This is a test message from qdrust.\nIf you can read this, the channel works.\nTime: {t}",
    );
    let payload = json!({
        "event": "test",
        "task_id": null,
        "task_name": null,
        "run_id": null,
        "http_status": null,
        "error": null,
        "title": title,
        "body": body,
    });
    let message = crate::delivery::Message {
        title: &title,
        body: &body,
        payload: &payload,
    };
    crate::delivery::deliver(
        &state.outbound,
        &channel.kind,
        &channel.config,
        &message,
        &|value| vars.render(value),
        None,
    )
    .await
    .map_err(ApiError::unprocessable)?;
    Ok(Json(json!({ "ok": true, "kind": channel.kind })))
}

async fn list_notification_actions(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let actions = store
        .list_notification_actions(id, session.user.id)
        .await?
        .ok_or(ApiError::NotFound("task_not_found", "Task not found"))?;
    Ok(Json(json!(actions)))
}

/// Every binding the caller owns, across all of their tasks. The notify page
/// wants one list of what is configured, so this replaces fanning out a
/// per-task request for whichever tasks happened to be ticked.
async fn list_all_notification_actions(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let actions = store.list_all_notification_actions(session.user.id).await?;
    Ok(Json(json!(actions)))
}

async fn create_notification_action(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<CreateNotificationAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let action = store
        .create_notification_action(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .ok_or(ApiError::NotFound(
            "task_or_channel_not_found",
            "Task or notification channel not found",
        ))?;
    Ok((StatusCode::CREATED, Json(json!(action))))
}

async fn batch_create_notification_actions(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<BatchCreateNotificationAction>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let created = store
        .create_notification_actions_for_tasks(session.user.id, &input.task_ids, input.action)
        .await
        .map_err(ApiError::unprocessable)?;
    Ok(Json(json!({"created": created})))
}

async fn delete_notification_action(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    if store
        .delete_notification_action(id, session.user.id)
        .await?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(
            "notification_action_not_found",
            "Notification action not found",
        ))
    }
}

/// Edit one binding without re-creating it, so a corrected template or a
/// re-bound channel keeps its identity and its history.
async fn update_notification_action(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdateNotificationAction>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .update_notification_action(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|v| Json(json!(v)))
        .ok_or(ApiError::NotFound(
            "notification_action_not_found",
            "Notification action not found",
        ))
}

/// The template list's query string — typed for the same reason as
/// [`RunsQuery`]: `cursor` and `limit` were read with `as_i64` off a
/// `serde_json::Value`, so both were always `None` and the cursor paging this
/// endpoint advertises never advanced.
#[derive(Deserialize, Default)]
struct TemplatesQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    grp: Option<String>,
    #[serde(default)]
    cursor: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn list_templates(
    State(store): State<Store>,
    headers: HeaderMap,
    Query(query): Query<TemplatesQuery>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let limit = query.limit.unwrap_or(50);
    let templates = store
        .search_templates_for_owner(
            session.user.id,
            query.q.as_deref(),
            query.grp.as_deref(),
            query.cursor,
            limit,
        )
        .await?;
    let has_more = templates.len() as i64 > limit;
    let items: Vec<Value> = if has_more {
        templates[..templates.len() - 1]
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
            .collect()
    } else {
        templates
            .into_iter()
            .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
            .collect()
    };
    let next_cursor = items
        .last()
        .and_then(|t| t.get("id"))
        .and_then(Value::as_i64);
    Ok(Json(json!({
        "items": items,
        "has_more": has_more,
        "next_cursor": if has_more { next_cursor } else { None },
    })))
}

async fn list_public_templates(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_session_from_store(&store, &headers).await?;
    Ok(Json(json!(store.list_public_templates().await?)))
}
async fn publish_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    if store.set_template_published(id, s.user.id, true).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        ))
    }
}
async fn unpublish_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    if store.set_template_published(id, s.user.id, false).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        ))
    }
}
async fn copy_public_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    let template = store
        .copy_public_template(id, s.user.id)
        .await?
        .ok_or(ApiError::NotFound(
            "public_template_not_found",
            "Public template not found",
        ))?;
    Ok((StatusCode::CREATED, Json(json!(template))))
}

async fn list_plugins(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    Ok(Json(json!(store.list_plugins(s.user.id).await?)))
}
async fn create_plugin(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreatePluginManifest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!(
            store
                .create_plugin(s.user.id, input)
                .await
                .map_err(ApiError::unprocessable)?
        )),
    ))
}
async fn get_plugin(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    store
        .get_plugin(id, s.user.id)
        .await?
        .map(|v| Json(json!(v)))
        .ok_or(ApiError::NotFound("plugin_not_found", "Plugin not found"))
}
async fn update_plugin(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdatePluginManifest>,
) -> Result<Json<Value>, ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    store
        .update_plugin(id, s.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|v| Json(json!(v)))
        .ok_or(ApiError::NotFound("plugin_not_found", "Plugin not found"))
}
async fn delete_plugin(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, s) = require_session_from_store(&store, &headers).await?;
    if store.delete_plugin(id, s.user.id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("plugin_not_found", "Plugin not found"))
    }
}

async fn invoke_plugin(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<InvokePlugin>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let plugin = store
        .get_plugin(id, session.user.id)
        .await?
        .filter(|p| p.enabled)
        .ok_or(ApiError::NotFound("plugin_not_found", "Plugin not found"))?;
    if input.action.trim().is_empty() {
        return Err(ApiError::unprocessable(anyhow::anyhow!(
            "plugin action cannot be empty"
        )));
    }
    // Capabilities declared in the plugin's config gate what the plugin may
    // report as used; `SubprocessPlugin::call` rejects anything undeclared, so
    // this ad-hoc route enforces the same contract as template execution.
    let capabilities = crate::model::plugin_capabilities(&plugin.config)
        .map_err(|err| ApiError::unprocessable(anyhow::anyhow!(err)))?;
    let runner = SubprocessPlugin::from_command(
        CorePluginManifest {
            api_version: PLUGIN_API_VERSION,
            id: format!("plugin-{id}"),
            name: plugin.name.clone(),
            version: "1".into(),
            capabilities,
        },
        &plugin.command,
    )
    .map_err(ApiError::unprocessable)?;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        runner.call(&PluginRequest {
            plugin_id: format!("plugin-{id}"),
            action: input.action.clone(),
            query: input.query,
        }),
    )
    .await
    .map_err(|_| ApiError::unprocessable(anyhow::anyhow!("plugin call timed out")))?
    .map_err(ApiError::unprocessable);
    store
        .record_audit(
            Some(session.user.id),
            if result.is_ok() {
                "plugin.invoke"
            } else {
                "plugin.invoke_failed"
            },
            Some("plugin"),
            Some(id),
            None,
            &json!({"action":input.action}),
        )
        .await?;
    let response = result?;
    Ok(Json(
        json!({"status":response.status,"headers":response.headers,"body":String::from_utf8_lossy(&response.body)}),
    ))
}

async fn create_template(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateTemplate>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!(
            store
                .create_template_for_owner(session.user.id, input)
                .await
                .map_err(ApiError::unprocessable)?
        )),
    ))
}

async fn import_qd_har(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<ImportQdHarTemplate>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!(
            store
                .import_qd_har_for_owner(session.user.id, input)
                .await
                .map_err(ApiError::unprocessable)?
        )),
    ))
}

async fn validate_qd_har(
    ApiJson(input): ApiJson<ValidateQdHar>,
) -> Result<Json<QdHarValidation>, ApiError> {
    let har = QdHar::parse_qd(input.har).map_err(ApiError::unprocessable)?;
    QdProgram::compile(&har).map_err(ApiError::unprocessable)?;
    let enabled = har.enabled_entries().count();
    let controls = har
        .enabled_entries()
        .filter(|entry| entry.control().is_some())
        .count();
    let extract_variables = har
        .enabled_entries()
        .map(|entry| entry.extract_variables.len())
        .sum();
    Ok(Json(QdHarValidation {
        valid: true,
        entries: har.entries().len(),
        enabled,
        requests: enabled - controls,
        controls,
        extract_variables,
    }))
}

/// Run a saved template with the caller's variables, without creating a task or
/// a run record.
///
/// The execution path is the scheduler's, not a second implementation: the same
/// per-run policy (timeouts and both ADR-0008 relaxations), the same
/// request/loop limits and the same SSRF guard apply. What differs is only that
/// the outcome comes back to the caller instead of being written to
/// `runs`/`run_steps`.
async fn test_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<TestTemplate>,
) -> Result<Json<TemplateTestResult>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let template = store
        .get_template_for_owner(id, session.user.id)
        .await?
        .ok_or(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        ))?;
    let policy = crate::scheduler::run_policy(None, &runtime_settings());
    let plugins = crate::scheduler::load_plugins_for_owner(&store, session.user.id, None).await;
    let (steps, variables) = crate::scheduler::execute_template(
        template,
        &CancellationToken::new(),
        &input.variables,
        policy,
        &plugins,
    )
    .await
    .map_err(ApiError::unprocessable)?;
    let steps = steps
        .into_iter()
        .enumerate()
        .map(|(index, step)| TemplateTestStep {
            index,
            url: step.url,
            status: step.status,
            body_size: step.body_size,
        })
        .collect();
    // `__log__` is the template's own summary line, the same one a real run
    // stores as its log.
    let log = variables
        .get("__log__")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    Ok(Json(TemplateTestResult {
        steps,
        variables,
        log,
    }))
}

async fn get_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .get_template_for_owner(id, session.user.id)
        .await?
        .map(|template| Json(json!(template)))
        .ok_or(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        ))
}

async fn update_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdateTemplate>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .update_template_for_owner(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|template| Json(json!(template)))
        .ok_or(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        ))
}

/// Delete one of the caller's templates.
///
/// `tasks.template_id` is declared `ON DELETE RESTRICT`, so a template that
/// still has tasks cannot be removed. Left to the database that shows up as a
/// foreign-key violation, which the blanket `From<E> for ApiError` folds into
/// `internal_error` -- a 500 telling the user nothing. Count first instead, and
/// refuse with a 409 that says what is holding the template and how much of it.
///
/// The ownership question is only asked when the count is non-zero: a template
/// with no tasks is settled by the delete itself (`0 rows affected` -> 404), and
/// a template that is not the caller's must stay a 404 rather than reporting
/// another owner's task count.
async fn delete_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;

    let bound = store.count_tasks_for_template(id).await?;
    if bound > 0 {
        if store
            .get_template_for_owner(id, session.user.id)
            .await?
            .is_none()
        {
            return Err(ApiError::NotFound(
                "template_not_found",
                "Template not found",
            ));
        }
        return Err(template_in_use(bound));
    }

    match store.delete_template_for_owner(id, session.user.id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        )),
        // A task was bound between the count above and this delete, so the
        // foreign key refused it after all. Ask the same question again and
        // answer with the real reason -- no driver-specific error strings, and
        // the race ends in the same 409 the caller would have got a moment
        // earlier instead of an opaque 500.
        Err(cause) => {
            let bound = store.count_tasks_for_template(id).await.unwrap_or(0);
            if bound > 0 {
                Err(template_in_use(bound))
            } else {
                Err(cause.into())
            }
        }
    }
}

/// The 409 for a template that cannot go yet. Shared by the check above and its
/// race backstop so both answer identically.
fn template_in_use(bound: i64) -> ApiError {
    ApiError::Conflict(
        "template_in_use",
        format!("Template is still used by {bound} task(s); delete or re-bind them first"),
    )
}

async fn update_qd_har(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdateQdHarTemplate>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .update_qd_har_for_owner(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|v| Json(json!(v)))
        .ok_or(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        ))
}

async fn register(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<RegisterUser>,
) -> Result<Response, ApiError> {
    ensure_local_login_allowed(&state)?;
    let password = input.password;
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(anyhow::Error::from)??;
    let user = state
        .store
        .create_user(input.username.trim(), &password_hash, "user")
        .await
        .map_err(|_| {
            ApiError::Conflict(
                "username_taken",
                "Username is already registered or invalid".into(),
            )
        })?;
    if let Some(email) = input.email.as_deref() {
        let _ = state.store.set_user_email(user.id, email).await;
        let (token, _expires) = state
            .store
            .create_email_verification_token(user.id, 3600)
            .await?;
        let email_client = crate::email::EmailClient::new(crate::email::EmailConfig::from_env())?;
        let base_url = std::env::var("QDRUST_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:8923".to_string());
        let verify_url = format!("{base_url}/verify-email?token={token}");
        let _ = email_client
            .send(
                email.trim(),
                None,
                "[qdrust] Verify your email",
                &format!(
                    "Hello {},\n\nVerify your email address by opening this link:\n{}\n\nIf you did not request this, you can ignore this message.\n",
                    user.username, verify_url
                ),
            )
            .ok();
    }
    state
        .store
        .record_audit(
            Some(user.id),
            "auth.register",
            Some("user"),
            Some(user.id),
            None,
            &json!({}),
        )
        .await?;
    issue_session_response(&state, user).await
}

async fn forgot_password(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<ForgotPassword>,
) -> Result<Json<Value>, ApiError> {
    ensure_local_login_allowed(&state)?;
    // Uniform response so username enumeration is not possible.
    let Ok(Some(user)) = state
        .store
        .credentials_by_username(input.username.trim())
        .await
    else {
        return Ok(Json(json!({"sent": true})));
    };
    if user.user.disabled {
        return Ok(Json(json!({"sent": true})));
    }
    let (token, expires_at) = state
        .store
        .create_password_reset_token(user.user.id, 3600)
        .await?;
    let base_url =
        std::env::var("QDRUST_BASE_URL").unwrap_or_else(|_| "http://localhost:8923".to_string());
    let reset_url = format!("{base_url}/reset-password?token={token}");
    // Email delivery is best-effort. When SMTP is configured the token is never
    // returned in the response (keeps the anti-enumeration contract and does not
    // leak reset tokens); without SMTP the token is exposed so local development
    // can complete the flow.
    let email_config = crate::email::EmailConfig::from_env();
    let smtp_configured = email_config.enabled();
    if smtp_configured
        && let Ok(email_client) = crate::email::EmailClient::new(email_config)
        && let Some(email) = user.user.email.as_deref()
        && let Err(err) = email_client.send(
            email.trim(),
            None,
            "[qdrust] Reset your password",
            &format!(
                "Hello {},\n\nReset your password by opening this link:\n{}\n\nIf you did not request this, you can ignore this message. The link expires in 1 hour.\n",
                user.user.username, reset_url
            ),
        )
    {
        tracing::warn!(%err, user_id = user.user.id, "password reset email delivery failed");
    }
    state
        .store
        .record_audit(
            Some(user.user.id),
            "auth.password_reset_requested",
            Some("user"),
            Some(user.user.id),
            None,
            &json!({}),
        )
        .await?;
    if smtp_configured {
        Ok(Json(json!({"sent": true})))
    } else {
        // Local development fallback: expose the token so the reset flow is
        // testable without an SMTP server.
        Ok(Json(json!({
            "sent": true,
            "expires_at": expires_at,
            "reset_token": token,
            "reset_url": reset_url,
        })))
    }
}

async fn reset_password(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<ResetPassword>,
) -> Result<Json<Value>, ApiError> {
    let new_password = input.new_password;
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&new_password))
        .await
        .map_err(anyhow::Error::from)??;
    let Some(user_id) = state
        .store
        .consume_password_reset_token(&input.token, &password_hash)
        .await?
    else {
        return Err(ApiError::Unauthorized(
            "invalid_or_expired_reset_token",
            "The reset token is invalid or has expired",
        ));
    };
    state
        .store
        .record_audit(
            Some(user_id),
            "auth.password_reset",
            Some("user"),
            Some(user_id),
            None,
            &json!({}),
        )
        .await?;
    Ok(Json(json!({"ok": true})))
}

async fn list_task_groups(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok(Json(json!(
        store.list_groups_for_owner(session.user.id).await?
    )))
}

async fn batch_tasks(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<BatchTaskOperation>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let updated = store
        .batch_operations_for_owner(session.user.id, &input)
        .await
        .map_err(ApiError::unprocessable)?;
    Ok(Json(json!(BatchTaskResult { updated })))
}

async fn run_steps_websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    // Ownership check before upgrading.
    if state
        .store
        .list_run_steps_for_owner(id, session.user.id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound("run_not_found", "Run not found"));
    }
    let store = state.store.clone();
    let events = state.run_events.subscribe();
    Ok(ws.on_upgrade(move |socket| stream_run_steps(socket, store, id, events)))
}

async fn stream_run_steps(
    mut socket: axum::extract::ws::WebSocket,
    store: Store,
    run_id: i64,
    mut events: tokio::sync::broadcast::Receiver<Value>,
) {
    use futures_util::SinkExt;
    // Send the initial snapshot of known steps.
    if let Ok(steps) = store.list_run_steps(run_id).await {
        let _ = socket
            .send(Message::Text(
                json!({"type": "snapshot", "run_id": run_id, "steps": steps})
                    .to_string()
                    .into(),
            ))
            .await;
    }
    // Stream subsequent live events for this run.
    loop {
        let event = tokio::select! {
            _ = socket.recv() => {
                // Client ping/close handling.
                break;
            },
            event = events.recv() => match event {
                Ok(value) => value,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            },
        };
        let ev_run_id = event.get("run_id").and_then(|v| v.as_i64());
        if ev_run_id != Some(run_id) {
            continue;
        }
        if socket
            .send(Message::Text(event.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
    let _ = socket.close().await;
}

async fn admin_list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers).await?;
    Ok(Json(json!(state.store.list_users().await?)))
}

async fn admin_update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<AdminUserUpdate>,
) -> Result<Json<Value>, ApiError> {
    let (_, admin) = require_admin(&state, &headers).await?;
    if admin.user.id == id {
        return Err(ApiError::Forbidden(
            "cannot_modify_self",
            "Cannot modify your own account here",
        ));
    }
    state
        .store
        .update_user(id, &input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|user| Json(json!(user)))
        .ok_or(ApiError::NotFound("user_not_found", "User not found"))
}

async fn admin_delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, admin) = require_admin(&state, &headers).await?;
    if admin.user.id == id {
        return Err(ApiError::Forbidden(
            "cannot_delete_self",
            "Cannot delete your own account",
        ));
    }
    if !state.store.delete_user(id).await? {
        return Err(ApiError::NotFound("user_not_found", "User not found"));
    }
    state
        .store
        .record_audit(
            Some(admin.user.id),
            "admin.user_deleted",
            Some("user"),
            Some(id),
            None,
            &json!({}),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn admin_list_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers).await?;
    Ok(Json(json!(state.store.list_settings().await?)))
}

async fn admin_get_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers).await?;
    state
        .store
        .get_setting(&key)
        .await?
        .map(|s| Json(json!(s)))
        .ok_or(ApiError::NotFound("setting_not_found", "Setting not found"))
}

async fn admin_set_setting(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    ApiJson(input): ApiJson<SetSiteSetting>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers).await?;
    ensure_setting_key(&key)?;
    let setting = state.store.set_setting(&key, &input).await?;
    // Apply it to the in-memory snapshot at once instead of waiting for the
    // settings watcher's next poll (up to 30s). A security toggle that stays
    // effective for another half minute after being switched off reads as a
    // bug; the watcher remains the path for keys written out-of-band and for
    // the other replicas, which this request cannot reach.
    apply_runtime_setting(&mut state.settings.write().unwrap(), &key, &input.value);
    state
        .store
        .record_audit(
            None,
            "admin.setting_changed",
            Some("setting"),
            None,
            None,
            &json!({"key": key}),
        )
        .await?;
    Ok(Json(json!(setting)))
}

async fn admin_clear_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<ClearLogs>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers).await?;
    let days = input.older_than_days.unwrap_or(0).max(0);
    let before = chrono::Utc::now().timestamp() - days * 86_400;
    match days {
        0 => {
            // Clear all finished run logs.
            let count = state.store.count_old_runs(before).await?;
            state.store.prune_run_logs(before).await?;
            Ok(Json(json!({"deleted": count})))
        }
        _ => {
            let count = state.store.prune_run_logs(before).await?;
            Ok(Json(json!({"deleted": count})))
        }
    }
}

fn ensure_setting_key(key: &str) -> Result<(), ApiError> {
    if key.is_empty() || key.len() > 128 {
        return Err(ApiError::unprocessable(anyhow::anyhow!(
            "setting key is invalid"
        )));
    }
    Ok(())
}

async fn require_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(String, AuthenticatedSession), ApiError> {
    let (token, session) = require_session(state, headers).await?;
    if session.user.role != "admin" {
        return Err(ApiError::Forbidden(
            "admin_required",
            "Administrator role required",
        ));
    }
    Ok((token, session))
}

// ==================== P1 features: email verification, CSRF rotation, subscriptions, push requests, backup ====================

async fn verify_email(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<VerifyEmail>,
) -> Result<Json<Value>, ApiError> {
    let Some(user_id) = state
        .store
        .consume_email_verification_token(&input.token)
        .await?
    else {
        return Err(ApiError::Unauthorized(
            "invalid_or_expired_verification_token",
            "The verification token is invalid or has expired",
        ));
    };
    state
        .store
        .record_audit(
            Some(user_id),
            "auth.email_verified",
            Some("user"),
            Some(user_id),
            None,
            &json!({}),
        )
        .await?;
    Ok(Json(json!({"ok": true, "user_id": user_id})))
}

async fn resend_verification(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let email = state
        .store
        .get_user(session.user.id)
        .await?
        .and_then(|u| u.email);
    let Some(email) = email else {
        return Err(ApiError::Unprocessable(anyhow::anyhow!(
            "no email address on this account"
        )));
    };
    let (token, expires_at) = state
        .store
        .create_email_verification_token(session.user.id, 3600)
        .await?;
    let base_url =
        std::env::var("QDRUST_BASE_URL").unwrap_or_else(|_| "http://localhost:8923".to_string());
    let verify_url = format!("{base_url}/verify-email?token={token}");
    let email_client = crate::email::EmailClient::new(crate::email::EmailConfig::from_env())?;
    let _ = email_client
        .send(
            &email,
            None,
            "[qdrust] Verify your email",
            &format!(
                "Hello {},\n\nVerify your email address by opening this link:\n{}\n\nIf you did not request this, you can ignore this message.\n",
                session.user.username, verify_url
            ),
        )
        .ok();
    Ok(Json(json!({
        "sent": true,
        "expires_at": expires_at,
        "verify_token": token,
    })))
}

async fn rotate_csrf_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (token, _session) = require_session(&state, &headers).await?;
    let new_csrf = crate::auth::new_token();
    state
        .store
        .rotate_csrf(&token, &crate::auth::token_hash(&new_csrf))
        .await?;
    let mut response = Json(json!({"csrf_token": new_csrf})).into_response();
    let max_age = state.auth.session_ttl.as_secs();
    let secure = if state.auth.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let cookie =
        format!("{CSRF_COOKIE}={new_csrf}; Path=/; SameSite=Strict; Max-Age={max_age}{secure}");
    response.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("static cookie attributes are valid"),
    );
    Ok(response)
}

async fn list_subscriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    Ok(Json(json!(
        state.store.list_subscriptions(session.user.id).await?
    )))
}

async fn create_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateTemplateSubscription>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let subscription = state
        .store
        .create_subscription(session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?;
    Ok((StatusCode::CREATED, Json(json!(subscription))))
}

async fn get_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    state
        .store
        .get_subscription(id, session.user.id)
        .await?
        .map(|s| Json(json!(s)))
        .ok_or(ApiError::NotFound(
            "subscription_not_found",
            "Subscription not found",
        ))
}

async fn update_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdateTemplateSubscription>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    state
        .store
        .update_subscription(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|s| Json(json!(s)))
        .ok_or(ApiError::NotFound(
            "subscription_not_found",
            "Subscription not found",
        ))
}

async fn delete_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    if state.store.delete_subscription(id, session.user.id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(
            "subscription_not_found",
            "Subscription not found",
        ))
    }
}

/// Catalogue a subscription source so the user can pick what to import. Reads
/// the source's `tpls_history.json` when it publishes one and otherwise scans
/// the repository tree.
async fn browse_subscription_library(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let subscription = owned_subscription(&state, id, session.user.id).await?;
    let library = crate::library::browse(&state.store, &state.outbound, &subscription)
        .await
        .map_err(ApiError::unprocessable)?;
    Ok(Json(json!(library)))
}

/// Import the selected entries of a source. Runs inline rather than in the
/// background because the caller picks a handful of templates and wants the
/// per-entry outcome back; entries that fail are reported individually so a
/// single broken upstream template does not discard the rest of the selection.
async fn import_subscription_library(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<ImportLibraryTemplates>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let subscription = owned_subscription(&state, id, session.user.id).await?;
    let result =
        crate::library::import_selected(&state.store, &state.outbound, &subscription, &input.names)
            .await
            .map_err(ApiError::unprocessable)?;
    Ok(Json(json!(result)))
}

/// One entry to fetch, named by the listing.
///
/// The name is a query parameter rather than a path segment because it is a
/// free-form string chosen by the source: a manifest key, or a repository file
/// path for a source with no manifest. A `/` in one would not survive a path
/// segment, and a reverse proxy is free to normalise an escaped one.
#[derive(Deserialize)]
struct LibraryEntryQuery {
    entry: String,
}

/// Fetch one entry's content without writing anything, so a template can be
/// read before it is worth a place in the user's library. This is the "look
/// first" half of subscribing; `apply_subscription_library` is the save.
#[derive(Deserialize, Default)]
struct RefreshQuery {
    #[serde(default)]
    refresh: bool,
}

async fn preview_subscription_library(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(params): Query<LibraryEntryQuery>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let subscription = owned_subscription(&state, id, session.user.id).await?;
    let preview = crate::library::preview(
        &state.store,
        &state.outbound,
        &state.catalogue_cache,
        &subscription,
        &params.entry,
    )
    .await
    .map_err(ApiError::unprocessable)?;
    Ok(Json(json!(preview)))
}

/// Save the template a user edited out of a preview. Runs inline because the
/// caller is waiting on exactly one template and wants its id back.
async fn apply_subscription_library(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<ApplyLibraryTemplate>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let subscription = owned_subscription(&state, id, session.user.id).await?;
    let outcome = crate::library::apply(
        &state.store,
        &state.outbound,
        &state.catalogue_cache,
        &subscription,
        input,
    )
    .await
    .map_err(ApiError::unprocessable)?;
    Ok(Json(json!(outcome)))
}

/// Every subscribed source in one list, which is what a QD user means by the
/// public templates page. Disabled subscriptions are left out — a source the
/// user turned off should not keep contributing rows they cannot act on.
///
/// `?refresh=true` re-reads the sources instead of reusing a recent catalogue,
/// for the user who just changed something upstream.
async fn library_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<RefreshQuery>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let subscriptions: Vec<TemplateSubscription> = state
        .store
        .list_subscriptions(session.user.id)
        .await?
        .into_iter()
        .filter(|subscription| subscription.enabled)
        .collect();
    let overview = crate::library::overview(
        &state.store,
        &state.outbound,
        &state.catalogue_cache,
        &subscriptions,
        params.refresh,
    )
    .await
    .map_err(ApiError::unprocessable)?;
    Ok(Json(json!(overview)))
}

async fn owned_subscription(
    state: &AppState,
    id: i64,
    owner_id: i64,
) -> Result<TemplateSubscription, ApiError> {
    state
        .store
        .get_subscription(id, owner_id)
        .await?
        .ok_or(ApiError::NotFound(
            "subscription_not_found",
            "Subscription not found",
        ))
}

async fn create_push_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreatePushRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let request = state
        .store
        .create_push_request(session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .ok_or(ApiError::NotFound(
            "template_not_found_or_already_public",
            "Template not found, already public, or note missing",
        ))?;
    state
        .store
        .record_audit(
            Some(session.user.id),
            "template.push_requested",
            Some("template"),
            Some(request.template_id),
            None,
            &json!({"push_request_id": request.id}),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(json!(request))))
}

async fn list_my_push_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    Ok(Json(json!(
        state
            .store
            .list_push_requests_for_owner(session.user.id)
            .await?
    )))
}

async fn list_admin_push_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
    // Safe only while every parameter here is read as text: a number would
    // arrive as a string and `as_i64` would find nothing — see `RunsQuery`.
    Query(params): Query<serde_json::Value>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers).await?;
    let status = params.get("status").and_then(|v| v.as_str());
    Ok(Json(json!(state.store.list_push_requests(status).await?)))
}

async fn decide_push_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<DecidePushRequest>,
) -> Result<Json<Value>, ApiError> {
    let (_, admin) = require_admin(&state, &headers).await?;
    let request = state
        .store
        .decide_push_request(id, admin.user.id, &input)
        .await?
        .ok_or(ApiError::NotFound(
            "push_request_not_found",
            "Push request not found",
        ))?;
    state
        .store
        .record_audit(
            Some(admin.user.id),
            if input.approve {
                "template.push_approved"
            } else {
                "template.push_rejected"
            },
            Some("push_request"),
            Some(request.id),
            None,
            &json!({}),
        )
        .await?;
    Ok(Json(json!(request)))
}

async fn admin_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_admin(&state, &headers).await?;
    let backup = state.store.export_data().await?;
    let body = serde_json::to_vec_pretty(&backup).map_err(anyhow::Error::from)?;
    let filename = format!(
        "qdrust-backup-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    Ok((
        StatusCode::OK,
        [
            (
                HeaderName::from_static("content-type"),
                HeaderValue::from_static("application/json"),
            ),
            (
                HeaderName::from_static("content-disposition"),
                HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                    .expect("valid header"),
            ),
        ],
        body,
    )
        .into_response())
}

async fn admin_restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<serde_json::Value>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers).await?;
    state
        .store
        .import_data(&input)
        .await
        .map_err(ApiError::unprocessable)?;
    state
        .store
        .record_audit(
            None,
            "admin.restore",
            Some("system"),
            None,
            None,
            &json!({}),
        )
        .await?;
    Ok(Json(json!({"ok": true})))
}

enum ApiError {
    NotFound(&'static str, &'static str),
    Unauthorized(&'static str, &'static str),
    Forbidden(&'static str, &'static str),
    /// A 409: the request is well-formed but the current state forbids it. The
    /// message is owned rather than `&'static str` because some conflicts can
    /// only be explained with a number in them — how many tasks still hold a
    /// template, for instance — and a client cannot be expected to guess that
    /// from a fixed sentence.
    Conflict(&'static str, String),
    TooManyRequests(&'static str, &'static str),
    Unprocessable(anyhow::Error),
    Internal(anyhow::Error),
}

struct ApiJson<T>(T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: AxumRequest, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|error: JsonRejection| {
                ApiError::unprocessable(anyhow::anyhow!(error.body_text()))
            })
    }
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
    field_errors: BTreeMap<String, Vec<String>>,
    request_id: String,
}

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

impl ApiError {
    fn unprocessable(error: impl Into<anyhow::Error>) -> Self {
        Self::Unprocessable(error.into())
    }
}
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(error: E) -> Self {
        Self::Internal(error.into())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = format!(
            "req-{:016x}",
            NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        );
        let (status, code, message) = match self {
            Self::NotFound(code, message) => (StatusCode::NOT_FOUND, code, message.into()),
            Self::Unauthorized(code, message) => (StatusCode::UNAUTHORIZED, code, message.into()),
            Self::Forbidden(code, message) => (StatusCode::FORBIDDEN, code, message.into()),
            Self::Conflict(code, message) => (StatusCode::CONFLICT, code, message),
            Self::TooManyRequests(code, message) => {
                (StatusCode::TOO_MANY_REQUESTS, code, message.into())
            }
            Self::Unprocessable(error) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_error",
                error.to_string(),
            ),
            Self::Internal(error) => {
                tracing::error!(%request_id, %error, "API request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "An internal error occurred".into(),
                )
            }
        };
        let mut response = (
            status,
            Json(ErrorBody {
                code,
                message,
                field_errors: BTreeMap::new(),
                request_id: request_id.clone(),
            }),
        )
            .into_response();
        response.headers_mut().insert(
            HeaderName::from_static("x-request-id"),
            HeaderValue::from_str(&request_id).expect("request id is a valid header"),
        );
        response
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Request, header::SET_COOKIE},
    };
    use tower::ServiceExt;

    use super::*;

    async fn test_app() -> Router {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        router(store)
    }

    async fn test_app_at(base_path: &str) -> Router {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        router_with_auth(
            store,
            AuthConfig::default(),
            run_event_channel().0,
            runtime_settings(),
            crate::outbound::OutboundHttp::standalone(),
            crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
            base_path,
            None,
        )
    }

    #[test]
    fn every_runtime_setting_key_maps_to_its_field() {
        // The mapping table is the only place a stored key turns into behaviour:
        // a key that is persisted but missing from it is accepted, audited and
        // then ignored. Each one is asserted here next to its own field.
        let mut runtime = RuntimeSettings::default();
        apply_runtime_setting(&mut runtime, "require_email_verification", &json!(true));
        apply_runtime_setting(&mut runtime, "ga_key", &json!("G-TEST"));
        apply_runtime_setting(&mut runtime, "logs.retention_days", &json!(30));
        apply_runtime_setting(&mut runtime, ALLOW_PRIVATE_NETWORK_SETTING, &json!(true));
        apply_runtime_setting(
            &mut runtime,
            ALLOW_INVALID_CERTIFICATES_SETTING,
            &json!(true),
        );

        assert!(runtime.require_email_verification);
        assert_eq!(runtime.ga_key.as_deref(), Some("G-TEST"));
        assert_eq!(runtime.log_retention_days, 30);
        assert!(runtime.allow_private_network);
        assert!(runtime.allow_invalid_certificates);
    }

    #[test]
    fn the_two_relaxation_switches_stay_independent() {
        // ADR-0008 asks for two switches, not one. Needing to reach a LAN host
        // is no reason to also stop verifying certificates, so turning one on
        // must not quietly turn the other on. This is the assertion that keeps
        // "implemented both" from decaying into "implemented them together".
        let mut runtime = RuntimeSettings::default();
        apply_runtime_setting(&mut runtime, ALLOW_PRIVATE_NETWORK_SETTING, &json!(true));
        assert!(runtime.allow_private_network);
        assert!(!runtime.allow_invalid_certificates);

        let mut runtime = RuntimeSettings::default();
        apply_runtime_setting(
            &mut runtime,
            ALLOW_INVALID_CERTIFICATES_SETTING,
            &json!(true),
        );
        assert!(runtime.allow_invalid_certificates);
        assert!(!runtime.allow_private_network);
    }

    #[test]
    fn the_relaxation_switches_fail_closed_on_a_malformed_value() {
        // Type-strict, like the other keys. Anything that is not a real JSON
        // boolean has to leave the guards on: a hand-written request sending
        // {"value": "true"} should hand out neither network access nor
        // certificate forgiveness.
        for key in [
            ALLOW_PRIVATE_NETWORK_SETTING,
            ALLOW_INVALID_CERTIFICATES_SETTING,
        ] {
            for value in [json!("true"), json!(1), json!(null), json!({})] {
                let mut runtime = RuntimeSettings::default();
                apply_runtime_setting(&mut runtime, key, &value);
                assert!(
                    !runtime.allow_private_network && !runtime.allow_invalid_certificates,
                    "{key} = {value} must not enable anything"
                );
            }
        }
    }

    #[test]
    fn every_relaxation_switch_is_wired_from_config_and_reloaded_by_the_poller() {
        // The two pieces of main.rs that no behavioural test reaches: the
        // start-up copy from the parsed config, and the poller's key list. The
        // list matters most — it is what re-applies a stored setting after a
        // restart, so a key missing from it makes the switch silently revert to
        // the deploy-time default on every boot.
        let main = include_str!("main.rs");
        for name in ["allow_private_network", "allow_invalid_certificates"] {
            let copy = format!("{}{}{}", "runtime.", name, " = config.");
            assert!(
                main.contains(&format!("{copy}{name};")),
                "main.rs must seed the runtime snapshot with {name} from the config"
            );
        }
        for constant in [
            "api::ALLOW_PRIVATE_NETWORK_SETTING",
            "api::ALLOW_INVALID_CERTIFICATES_SETTING",
        ] {
            assert!(
                main.contains(constant),
                "{constant} must be in the settings poller's key list"
            );
        }
    }

    fn public(auth_mode: &'static str, oidc: bool, local: bool) -> crate::config::PublicAuthConfig {
        crate::config::PublicAuthConfig {
            auth_mode,
            local_login_enabled: local,
            oidc_enabled: oidc,
            oidc_provider_name: if oidc {
                "Authentik".to_string()
            } else {
                String::new()
            },
            header_auth_enabled: false,
            oidc_logout_url: String::new(),
            oidc_post_logout_redirect_uri: String::new(),
        }
    }

    async fn test_app_with_public(public: crate::config::PublicAuthConfig) -> Router {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        router_with_auth(
            store,
            AuthConfig {
                public,
                ..AuthConfig::default()
            },
            run_event_channel().0,
            runtime_settings(),
            crate::outbound::OutboundHttp::standalone(),
            crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
            "",
            None,
        )
    }

    #[tokio::test]
    async fn auth_config_exposes_only_public_fields() {
        let app = test_app_with_public(public("oidc", true, false)).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["auth_mode"], "oidc");
        assert_eq!(value["local_login_enabled"], false);
        assert_eq!(value["oidc_enabled"], true);
        assert_eq!(value["oidc_provider_name"], "Authentik");
        assert_eq!(value["header_auth_enabled"], false);
        assert_eq!(value["oidc_logout_url"], "");
        assert_eq!(value["oidc_post_logout_redirect_uri"], "");
        // No secret material may ever appear in this response.
        assert!(!text.to_lowercase().contains("secret"));
        assert!(!text.to_lowercase().contains("client_id"));
    }

    /// A locale-parameterised app for the pre-login metadata endpoint.
    async fn test_app_with_meta(meta: crate::config::PublicMeta) -> Router {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        router_with_auth(
            store,
            AuthConfig {
                meta,
                ..AuthConfig::default()
            },
            run_event_channel().0,
            runtime_settings(),
            crate::outbound::OutboundHttp::standalone(),
            crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
            "",
            None,
        )
    }

    #[tokio::test]
    async fn meta_serves_the_deployments_default_language() {
        let app = test_app_with_meta(crate::config::PublicMeta {
            version: env!("CARGO_PKG_VERSION"),
            default_locale: "en-US".into(),
        })
        .await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/meta")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // No session, no cookie: the WebUI asks for this before anyone logs in.
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["default_locale"], "en-US");

        // The built-in default has to be a language the WebUI actually ships.
        let shipped = test_app_with_meta(crate::config::PublicMeta::default()).await;
        let response = shipped
            .oneshot(
                Request::builder()
                    .uri("/api/v1/meta")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 16 * 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["default_locale"], "zh-CN");
    }

    #[tokio::test]
    async fn auth_config_defaults_to_local_with_login_enabled() {
        let app = test_app().await; // AuthConfig::default()
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["auth_mode"], "local");
        assert_eq!(value["local_login_enabled"], true);
    }

    #[tokio::test]
    async fn local_login_disabled_rejects_entry_points_but_keeps_session_api() {
        // oidc-mode: local entry closed.
        let app = test_app_with_public(public("oidc", true, false)).await;

        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"username":"a","password":"b"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(login.into_body(), 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("local_login_disabled"));

        let register = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"username":"newbie","password":"passw0rd!","email":null}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(register.status(), StatusCode::FORBIDDEN);

        let forgot = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/forgot-password")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"username":"nobody"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forgot.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn local_login_disabled_forced_back_on_allows_login_endpoint() {
        // oidc-mode but local force-enabled (emergency backdoor).
        let app = test_app_with_public(public("oidc", true, true)).await;
        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"username":"a","password":"b"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Guard passes (local enabled) -> normal login flow runs, which for a
        // missing user returns a generic 401/400, NOT the disable 403.
        assert_ne!(login.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn health_and_ready_stay_at_root_when_nested() {
        // When served under a sub-path, liveness/readiness probes must remain
        // reachable at the bare root (Docker HEALTHCHECK / orchestrators).
        let app = test_app_at("/qd").await;
        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let ready = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ready.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_is_served_under_base_path_not_at_root() {
        let app = test_app_at("/qd").await;
        // Under the sub-path, an unknown API route returns the API's JSON 404
        // contract (routing reached the API, not the SPA fallback).
        let nested = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/qd/api/v1/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, _, body) = response_json(nested).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "api_endpoint_not_found");

        // At the bare root the same API path is NOT routed to the API handler:
        // the `/api/{*path}` catch-all lives only inside the nested sub-tree,
        // so a bare request must not yield the API's JSON error contract.
        let bare = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bare_body = to_bytes(bare.into_body(), 1024).await.unwrap();
        // The bare response must not carry the API 404 contract (`code`). It may
        // be non-JSON (empty / HTML), which is fine: it just isn't the API.
        if let Ok(value) = serde_json::from_slice::<Value>(&bare_body) {
            assert_ne!(value["code"], "api_endpoint_not_found");
        }
    }

    #[tokio::test]
    async fn nested_root_trailing_slash_is_served_like_the_bare_prefix() {
        // Regression: `Router::nest("/qd", ...)` does not itself match `/qd/`
        // (the trailing-slash root a browser/nginx sends). An outer SPA fallback
        // must catch it so the root-with-slash is served identically to `/qd`.
        // We assert the two forms return the same status and are both HTML-like
        // (SPA handling) rather than one being an empty axum 404. Whether the
        // on-disk index.html is reachable depends on the test cwd, so compare
        // relative behaviour instead of pinning a concrete status.
        let app = test_app_at("/qd").await;
        let bare_prefix = app
            .clone()
            .oneshot(Request::builder().uri("/qd").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let trailing_slash = app
            .clone()
            .oneshot(Request::builder().uri("/qd/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(bare_prefix.status(), trailing_slash.status());
        // Neither may carry the API 404 contract (i.e. they are served by the
        // SPA path, not leaked through the API catch-all).
        for res in [bare_prefix, trailing_slash] {
            let body = to_bytes(res.into_body(), 1024).await.unwrap();
            if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                assert_ne!(value["code"], "api_endpoint_not_found");
            }
        }
    }

    async fn response_json(response: Response) -> (StatusCode, HeaderValue, Value) {
        let status = response.status();
        let request_id = response.headers().get("x-request-id").unwrap().clone();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, request_id, serde_json::from_slice(&body).unwrap())
    }

    fn response_cookies(response: &Response) -> Vec<String> {
        response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }

    async fn test_auth_cookie(app: &Router) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/bootstrap")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "username": "route_admin",
                            "password": "correct horse battery staple"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response_cookies(&response).join("; ")
    }

    #[tokio::test]
    async fn unknown_api_uses_stable_error_contract() {
        let response = test_app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/api/v1/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, request_id, body) = response_json(response).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "api_endpoint_not_found");
        assert_eq!(body["request_id"], request_id.to_str().unwrap());
        assert!(body["field_errors"].as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn bootstrap_session_and_csrf_logout_flow() {
        let app = test_app().await;
        let bootstrap_body = json!({
            "username": "admin_user",
            "password": "correct horse battery staple"
        })
        .to_string();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/bootstrap")
                    .header("content-type", "application/json")
                    .body(Body::from(bootstrap_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookies = response_cookies(&response);
        assert_eq!(cookies.len(), 2);
        assert!(
            response
                .headers()
                .get_all(SET_COOKIE)
                .iter()
                .any(|value| value.to_str().unwrap().contains("HttpOnly"))
        );
        let cookie_header = cookies.join("; ");
        let csrf = cookies
            .iter()
            .find_map(|cookie| cookie.strip_prefix("qd_csrf="))
            .unwrap()
            .to_owned();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/session")
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, _, body) = response_json(response).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["code"], "csrf_validation_failed");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header(COOKIE, &cookie_header)
                    .header("x-csrf-token", csrf)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers().get_all(SET_COOKIE).iter().count(), 2);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/session")
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(bootstrap_body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get_all(SET_COOKIE).iter().count(), 2);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "username": "admin_user",
                            "password": "this password is incorrect"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, _, body) = response_json(response).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "invalid_credentials");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/bootstrap")
                    .header("content-type", "application/json")
                    .body(Body::from(bootstrap_body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn invalid_task_returns_validation_error() {
        let app = test_app().await;
        let cookie = test_auth_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tasks")
                    .header("content-type", "application/json")
                    .header(COOKIE, cookie)
                    .body(Body::from(
                        json!({
                            "name": "bad",
                            "cron": "not a cron",
                            "url": "https://example.invalid"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, _, body) = response_json(response).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "validation_error");
        assert!(body["message"].as_str().unwrap().contains("invalid cron"));
    }

    #[tokio::test]
    async fn malformed_json_uses_stable_error_contract() {
        let app = test_app().await;
        let cookie = test_auth_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/tasks")
                    .header("content-type", "application/json")
                    .header(COOKIE, cookie)
                    .body(Body::from("{"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let (status, request_id, body) = response_json(response).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "validation_error");
        assert_eq!(body["request_id"], request_id.to_str().unwrap());
    }

    #[tokio::test]
    async fn serves_openapi_contract() {
        let response = test_app()
            .await
            .oneshot(
                Request::builder()
                    .uri("/api/v1/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let document: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
        // The OpenAPI document is hand-maintained; its version must move with
        // the crate (scripts/bump-version.py keeps them in sync).
        assert_eq!(
            document["info"]["version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
        assert!(document["paths"]["/api/v1/tasks"].is_object());
        assert!(document["paths"]["/api/v1/templates/{id}/test"].is_object());
        // The WebUI reads the deployment's default language from here before it
        // mounts, so the path must stay in the published contract.
        assert!(document["paths"]["/api/v1/meta"]["get"].is_object());
        // The endpoints that back the WebUI's aggregated log, the channel-test
        // button and the notification editors must stay in the published
        // contract.
        assert!(document["paths"]["/api/v1/runs"]["get"].is_object());
        assert!(document["paths"]["/api/v1/notification-channels/{id}/test"]["post"].is_object());
        assert!(document["paths"]["/api/v1/notification-channels/{id}"]["put"].is_object());
        assert!(document["paths"]["/api/v1/notification-actions/{id}"]["put"].is_object());
        // The notify page reads its whole binding list from here rather than
        // one request per task.
        assert!(document["paths"]["/api/v1/notification-actions"]["get"].is_object());
        // Importing from a library returns the ids it wrote, so the WebUI can
        // open the editor on the template it just pulled in.
        assert_eq!(
            document["components"]["schemas"]["LibraryImportResult"]["properties"]["templates"]["items"]
                ["$ref"],
            json!("#/components/schemas/LibraryImportOutcome")
        );
        // Looking before keeping: preview reads the entry, apply saves it, and
        // the aggregate listing is what puts every source on one page.
        assert!(document["paths"]["/api/v1/subscriptions/{id}/library/preview"]["get"].is_object());
        assert!(document["paths"]["/api/v1/subscriptions/{id}/library/apply"]["post"].is_object());
        assert!(document["paths"]["/api/v1/library"]["get"].is_object());
        assert_eq!(
            document["components"]["schemas"]["LibraryPreview"]["properties"]["har"]["type"],
            json!("object")
        );
        assert_eq!(
            document["components"]["schemas"]["LibraryOverview"]["properties"]["sources"]["items"]
                ["$ref"],
            json!("#/components/schemas/LibrarySourceStatus")
        );
        assert_eq!(
            document["components"]["schemas"]["ApiError"]["required"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    /// The aggregated run log is owner-scoped and paginated; the channel test
    /// refuses a channel the caller does not own. Both are new routes, so this
    /// also proves they are actually mounted.
    #[tokio::test]
    async fn run_log_page_and_channel_test_are_wired() {
        let app = test_app().await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/bootstrap")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"username": "runs_admin", "password": "correct horse battery staple"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie_header = response_cookies(&response).join("; ");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/runs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/runs?status=failed&limit=10")
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // `response_json` needs the `x-request-id` header, which only ApiError
        // responses carry, so read this success body directly.
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["items"], json!([]));
        assert_eq!(body["has_more"], false);
        assert_eq!(body["next_cursor"], Value::Null);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/notification-channels/9999/test")
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Editing a binding validates the body before it resolves ownership, so a
    /// malformed payload fails the same way for everyone; both behaviours are
    /// new, which also proves the route is mounted.
    #[tokio::test]
    async fn updating_a_notification_action_is_validated_and_owner_scoped() {
        let app = test_app().await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/notification-actions/1")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"event": "sometimes"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let cookie = test_auth_cookie(&app).await;
        for body in [
            json!({"event": "sometimes"}),   // not one of the three events
            json!({"failure_threshold": 0}), // threshold must stay >= 1
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri("/api/v1/notification-actions/9999")
                        .header("content-type", "application/json")
                        .header(COOKIE, &cookie)
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let (status, _, payload) = response_json(response).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(payload["code"], "validation_error");
        }

        // A well-formed body against an id the caller does not have is a 404.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/notification-actions/9999")
                    .header("content-type", "application/json")
                    .header(COOKIE, &cookie)
                    .body(Body::from(json!({"event": "success"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// The notify page reads its whole binding list from this one endpoint, so
    /// it has to exist — and, since it is a collection of the caller's own rows
    /// rather than a task's, it must refuse an anonymous caller.
    #[tokio::test]
    async fn all_notification_actions_route_is_wired_and_authenticated() {
        let app = test_app().await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/notification-actions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let cookie = test_auth_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/notification-actions")
                    .header(COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!([]));
    }

    #[tokio::test]
    async fn subscription_library_routes_are_wired_and_owner_scoped() {
        let app = test_app().await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/bootstrap")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"username": "library_admin", "password": "correct horse battery staple"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie_header = response_cookies(&response).join("; ");

        // Both routes are session-gated.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/subscriptions/1/library")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Ownership is resolved before the source is contacted, so an unknown
        // id answers without any outbound request.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/subscriptions/9999/library")
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/subscriptions/9999/import")
                    .header(COOKIE, &cookie_header)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"names": ["雨晨分享站"]}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // A new subscription browses its source instead of importing all of it.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/subscriptions")
                    .header(COOKIE, &cookie_header)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "official",
                            "url": "https://github.com/qd-today/templates"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let created: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            created["url"],
            json!("https://github.com/qd-today/templates")
        );
        let id = created["id"].as_i64().unwrap();

        // An empty selection is refused before the source is contacted, so this
        // asserts the guard rather than the network.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/subscriptions/{id}/import"))
                    .header(COOKIE, &cookie_header)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"names": []}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("no templates selected")
        );

        // Preview and apply resolve ownership before the source is contacted
        // too, so an unknown id answers 404 with no outbound request. The
        // bodies are well formed on purpose: extractors run before the handler,
        // and a malformed one would answer 422 instead of exercising this.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/subscriptions/9999/library/preview?entry=whatever")
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/subscriptions/9999/library/apply")
                    .header(COOKIE, &cookie_header)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"entry": "whatever", "name": "whatever", "har": {}}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // The entry name is required, and it travels as a query parameter
        // rather than a path segment so a name containing a slash survives.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/subscriptions/{id}/library/preview"))
                    .header(COOKIE, &cookie_header)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // The aggregate listing is session-gated. It is deliberately not called
        // with a session here: this account now owns a subscription, and that
        // request would read GitHub.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/library")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn validates_qd_har_through_core() {
        let har: Value =
            serde_json::from_str(include_str!("../../../tests/fixtures/qd-basic.har")).unwrap();
        let response = test_app()
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/templates/validate-qd-har")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"har": har}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["valid"], true);
        assert_eq!(result["entries"], 4);
        assert_eq!(result["requests"], 2);
        assert_eq!(result["controls"], 2);
        assert_eq!(result["extract_variables"], 1);
    }

    #[tokio::test]
    async fn tests_a_saved_template_without_creating_a_task() {
        let app = test_app().await;
        let cookie = test_auth_cookie(&app).await;

        // The only step is an in-process util call, so the test touches no
        // network — the point here is the endpoint, not the HTTP stack.
        let har = json!({
            "log": {"version": "1.2", "entries": [
                {"checked": true, "request": {"method": "GET", "url": "api://util/delay?seconds=0"}}
            ]}
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/templates/import-qd-har")
                    .header("content-type", "application/json")
                    .header(COOKIE, &cookie)
                    .body(Body::from(
                        json!({"name": "probe", "description": null, "har": har}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        // Success responses do not carry `x-request-id` (only `ApiError` does),
        // so read the body directly rather than through `response_json`.
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let created: Value = serde_json::from_slice(&body).unwrap();
        let id = created["id"].as_i64().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/templates/{id}/test"))
                    .header("content-type", "application/json")
                    .header(COOKIE, &cookie)
                    .body(Body::from(json!({"variables": {}}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["steps"].as_array().unwrap().len(), 1);
        assert_eq!(body["steps"][0]["status"], 200);

        // A test run is not a task run: it must leave both tables untouched.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/tasks")
                    .header(COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let tasks: Value = serde_json::from_slice(&body).unwrap();
        assert!(tasks.as_array().unwrap().is_empty());
    }

    // --- External identity login audit trail (docs/design/EXTERNAL_IDP_PLAN.md Phase 2) ---

    fn external_claim(subject: &str, email: Option<&str>) -> ExternalIdentityClaim {
        ExternalIdentityClaim {
            provider: "oidc".into(),
            issuer: "https://issuer.example".into(),
            subject: subject.into(),
            email: email.map(Into::into),
            username_hint: Some(subject.into()),
            groups: vec![],
        }
    }

    async fn audit_actions_for(store: &crate::store::Store, actor: Option<i64>) -> Vec<String> {
        let pool = store.sqlite_pool();
        let rows: Vec<String> = if let Some(id) = actor {
            sqlx::query_scalar("SELECT action FROM audit_logs WHERE actor_user_id=? ORDER BY id")
                .bind(id)
                .fetch_all(pool)
                .await
                .unwrap()
        } else {
            sqlx::query_scalar(
                "SELECT action FROM audit_logs WHERE actor_user_id IS NULL ORDER BY id",
            )
            .fetch_all(pool)
            .await
            .unwrap()
        };
        rows
    }

    #[tokio::test]
    async fn external_login_writes_created_and_reused_audit() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let claim = external_claim("audit-sub", Some("audit@example.com"));

        // First login provisions a new user -> created audit action.
        let resolution = store
            .resolve_external_identity(&claim, true, "user", &[])
            .await
            .unwrap();
        let user = resolution.user.as_ref().unwrap();
        assert!(resolution.created);
        record_external_login_audit(&store, &claim, true, user)
            .await
            .unwrap();
        let actions = audit_actions_for(&store, Some(user.id)).await;
        assert_eq!(actions, vec!["auth.external_user_created"]);

        // Re-login reuses the same user -> plain external_login action.
        let again = store
            .resolve_external_identity(&claim, true, "user", &[])
            .await
            .unwrap();
        assert!(!again.created);
        record_external_login_audit(&store, &claim, false, again.user.as_ref().unwrap())
            .await
            .unwrap();
        let actions = audit_actions_for(&store, Some(user.id)).await;
        assert_eq!(
            actions,
            vec!["auth.external_user_created", "auth.external_login"]
        );
    }

    #[tokio::test]
    async fn external_conflict_writes_refused_audit_with_claimed_email() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        // A local user owns the claimed email.
        let owner = store
            .create_user(
                "owner",
                &crate::auth::hash_password("correct horse battery").unwrap(),
                "user",
            )
            .await
            .unwrap();
        store
            .set_user_email(owner.id, "claimed@example.com")
            .await
            .unwrap();

        // A brand-new external subject claiming that email is refused.
        let claim = external_claim("attacker-sub", Some("claimed@example.com"));
        let resolution = store
            .resolve_external_identity(&claim, true, "user", &[])
            .await
            .unwrap();
        assert!(resolution.user.is_none());
        assert_eq!(
            resolution.refusal.as_deref(),
            Some("external_identity_conflict")
        );

        // Mirror the refused audit the handler writes (actor None + details).
        store
            .record_audit(
                None,
                "auth.external_refused",
                None,
                None,
                None,
                &json!({
                    "reason": "external_identity_conflict",
                    "provider": claim.provider,
                    "issuer": claim.issuer,
                    "subject": claim.subject,
                    "email": claim.email,
                }),
            )
            .await
            .unwrap();

        let refused = audit_actions_for(&store, None).await;
        assert!(refused.contains(&"auth.external_refused".to_string()));
        // No extra local user row was created for the external subject.
        assert_eq!(store.list_users().await.unwrap().len(), 1);
    }

    // ---------------------------------------------------------------------
    // Header Auth (Phase 4) tests
    // ---------------------------------------------------------------------

    async fn header_auth_test_app(cfg: crate::config::HeaderAuthConfig) -> Router {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        router_with_auth(
            store,
            AuthConfig::default(),
            run_event_channel().0,
            runtime_settings(),
            crate::outbound::OutboundHttp::standalone(),
            crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
            "",
            Some(std::sync::Arc::new(cfg)),
        )
    }

    /// Build a GET request carrying the given identity headers and (optionally) a
    /// trusted-source `ConnectInfo`. Without an address the middleware treats the
    /// source as untrusted (mirrors a real oneshot request with no make-service).
    fn header_request(
        path: &str,
        headers: &[(&str, &str)],
        addr: Option<std::net::SocketAddr>,
    ) -> Request<Body> {
        let mut builder = Request::builder().uri(path).method("GET");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let mut req = builder.body(Body::empty()).unwrap();
        if let Some(a) = addr {
            req.extensions_mut().insert(ConnectInfo(a));
        }
        req
    }

    fn cookie_from_response(response: &Response, name: &str) -> Option<String> {
        for value in response.headers().get_all(SET_COOKIE).iter() {
            let Ok(s) = value.to_str() else { continue };
            if let Some(rest) = s.strip_prefix(&format!("{name}=")) {
                return Some(rest.split(';').next().unwrap().to_string());
            }
        }
        None
    }

    fn trusted_addr() -> std::net::SocketAddr {
        "127.0.0.1:54321".parse().unwrap()
    }

    fn untrusted_addr() -> std::net::SocketAddr {
        "10.0.0.9:54321".parse().unwrap()
    }

    fn header_cfg(trusted: bool) -> crate::config::HeaderAuthConfig {
        crate::config::HeaderAuthConfig {
            trusted_proxies: if trusted {
                vec!["127.0.0.1".parse().unwrap()]
            } else {
                vec![]
            },
            trusted_proxy_required: true,
            auto_create_users: true,
            ..crate::config::HeaderAuthConfig::new()
        }
    }

    #[tokio::test]
    async fn header_auth_disabled_is_transparent_without_session() {
        // Header auth is None (default router()); identity headers + trusted
        // source must NOT establish a session.
        let app = test_app().await;
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "alice"), ("Remote-Email", "a@x.com")],
                Some(trusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn untrusted_source_with_headers_is_403() {
        let app = header_auth_test_app(header_cfg(true)).await;
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "alice")],
                Some(untrusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn untrusted_source_when_not_required_is_ignored() {
        let mut cfg = header_cfg(true);
        cfg.trusted_proxy_required = false;
        let app = header_auth_test_app(cfg).await;
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "alice")],
                Some(untrusted_addr()),
            ))
            .await
            .unwrap();
        // No session established -> normal auth still applies (401, not 403).
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn missing_username_header_establishes_no_session() {
        let app = header_auth_test_app(header_cfg(true)).await;
        // Only email present -> no usable identity -> no session.
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-Email", "a@x.com")],
                Some(trusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn trusted_source_establishes_session_then_cookie_authenticates() {
        let app = header_auth_test_app(header_cfg(true)).await;
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "alice"), ("Remote-Email", "alice@x.com")],
                Some(trusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let token = cookie_from_response(&resp, "qd_session");
        assert!(token.is_some(), "a session cookie must be issued");

        // Subsequent request carrying the cookie (no headers) is authenticated.
        let cookie = format!("qd_session={}", token.unwrap());
        let resp2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/tasks")
                    .header("Cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auto_create_off_unknown_user_is_refused() {
        let mut cfg = header_cfg(true);
        cfg.auto_create_users = false;
        let app = header_auth_test_app(cfg).await;
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "ghost")],
                Some(trusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn existing_session_is_reused_not_recreated() {
        let app = header_auth_test_app(header_cfg(true)).await;
        // First request establishes the session.
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "alice")],
                Some(trusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let token = cookie_from_response(&resp, "qd_session").unwrap();
        let cookie = format!("qd_session={token}");

        // Second request: same trusted source, same header, AND the cookie.
        // The middleware must reuse the session (no new Set-Cookie issued).
        let mut req2 = Request::builder()
            .uri("/api/v1/tasks")
            .header("Cookie", &cookie)
            .header("Remote-User", "alice")
            .body(Body::empty())
            .unwrap();
        req2.extensions_mut().insert(ConnectInfo(trusted_addr()));
        let resp2 = app.clone().oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
        assert!(
            cookie_from_response(&resp2, "qd_session").is_none(),
            "reused session must not mint a new qd_session cookie"
        );
    }

    #[tokio::test]
    async fn same_email_claimed_by_another_user_is_refused() {
        let (app, store) = {
            let s = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
            let app = router_with_auth(
                s.clone(),
                AuthConfig::default(),
                run_event_channel().0,
                runtime_settings(),
                crate::outbound::OutboundHttp::standalone(),
                crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
                "",
                Some(std::sync::Arc::new(header_cfg(true))),
            );
            (app, s)
        };
        // A pre-existing local user owns alice@x.com.
        let hash = crate::auth::hash_password("local-pass-123").unwrap();
        store.create_user("localuser", &hash, "user").await.unwrap();
        store.set_user_email(1, "alice@x.com").await.unwrap();

        // A header identity for "bob" claiming that email must be refused
        // (never auto-merge into the existing account).
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "bob"), ("Remote-Email", "alice@x.com")],
                Some(trusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        // No external user was provisioned.
        assert_eq!(store.list_users().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn header_session_logout_clears_only_qdrust_session() {
        let app = header_auth_test_app(header_cfg(true)).await;
        let resp = app
            .clone()
            .oneshot(header_request(
                "/api/v1/tasks",
                &[("Remote-User", "alice")],
                Some(trusted_addr()),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let token = cookie_from_response(&resp, "qd_session").unwrap();
        let csrf = cookie_from_response(&resp, "qd_csrf").unwrap();

        // Logout (POST /api/v1/auth/logout) carries the session + csrf.
        let logout = Request::builder()
            .uri("/api/v1/auth/logout")
            .method("POST")
            .header("Cookie", format!("qd_session={token}; qd_csrf={csrf}"))
            .header("x-csrf-token", &csrf)
            .body(Body::empty())
            .unwrap();
        let logout_resp = app.clone().oneshot(logout).await.unwrap();
        assert_eq!(logout_resp.status(), StatusCode::NO_CONTENT);

        // The revoked token no longer authenticates.
        let after = Request::builder()
            .uri("/api/v1/tasks")
            .header("Cookie", format!("qd_session={token}"))
            .body(Body::empty())
            .unwrap();
        let after_resp = app.clone().oneshot(after).await.unwrap();
        assert_eq!(after_resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// A task with nothing but an identity: enough for tests that seed their own
    /// runs rather than going through the executor.
    fn seeded_task(name: &str) -> crate::model::CreateTask {
        crate::model::CreateTask {
            name: name.into(),
            cron: "0 * * * * *".into(),
            method: Some("GET".into()),
            url: "https://example.com/health".into(),
            headers: serde_json::Map::new(),
            body: None,
            disabled: false,
            template_id: None,
            grp: None,
            timeout_seconds: None,
            retry_count: None,
            retry_interval_seconds: None,
            priority: None,
            timezone: None,
            random_delay_max_seconds: None,
            variables: None,
        }
    }

    /// A template with no variables and one GET step: enough to be stored and
    /// bound to a task.
    fn seeded_template(name: &str) -> crate::model::CreateTemplate {
        use qdrust_core::template::{
            RequestStep, Step, TEMPLATE_SCHEMA_VERSION, TemplateDefinition,
        };
        crate::model::CreateTemplate {
            name: name.into(),
            description: None,
            definition: TemplateDefinition {
                version: TEMPLATE_SCHEMA_VERSION,
                name: name.into(),
                variables: Default::default(),
                steps: vec![Step::Request(RequestStep {
                    name: "request".into(),
                    method: "GET".into(),
                    url: "https://example.invalid/health".into(),
                    headers: Default::default(),
                    query: Default::default(),
                    body: None,
                })],
            },
            grp: None,
        }
    }

    async fn delete_template_request(app: &Router, cookie: &str, id: i64) -> Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/templates/{id}"))
                    .header(COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// Deleting a template that still has tasks must explain itself.
    ///
    /// `tasks.template_id` is `ON DELETE RESTRICT`, so the database refuses the
    /// delete. That refusal is a `sqlx::Error`, and the blanket
    /// `From<E> for ApiError` turns *every* such error into `internal_error` —
    /// so this used to answer `500 {"message":"An internal error occurred"}`
    /// with the real reason only in the server log. It now answers 409 and says
    /// how many tasks are holding the template, which is what the WebUI shows
    /// before it lets the user try.
    #[tokio::test]
    async fn deleting_a_template_that_still_has_tasks_is_a_conflict() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let app = router(store.clone());
        let cookie = test_auth_cookie(&app).await;
        let owner = store
            .list_users()
            .await
            .unwrap()
            .into_iter()
            .find(|user| user.username == "route_admin")
            .expect("bootstrap created the owner");
        let template = store
            .create_template_for_owner(owner.id, seeded_template("still-used"))
            .await
            .unwrap();
        let mut task = seeded_task("holds-the-template");
        task.template_id = Some(template.id);
        task.url = String::new();
        let task = store.create_for_owner(owner.id, task).await.unwrap();

        // The list the WebUI reads already carries the count, which is what the
        // delete button warns from -- no extra request needed.
        let listed = store.list_templates_for_owner(owner.id).await.unwrap();
        let row = listed.iter().find(|t| t.id == template.id).unwrap();
        assert_eq!(row.task_count, 1);

        let response = delete_template_request(&app, &cookie, template.id).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let (_, _, body) = response_json(response).await;
        assert_eq!(body["code"], "template_in_use");
        let message = body["message"].as_str().unwrap();
        assert!(
            message.contains('1'),
            "the message must say how many tasks are in the way: {message}"
        );

        // Nothing was deleted, on either side.
        assert!(
            store
                .get_template_for_owner(template.id, owner.id)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store.count_tasks_for_template(template.id).await.unwrap(),
            1
        );

        // Release it and the very same request succeeds.
        assert!(store.delete_for_owner(task.id, owner.id).await.unwrap());
        assert_eq!(
            delete_template_request(&app, &cookie, template.id)
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    /// The 409 must not become an oracle for another owner's templates: someone
    /// else's in-use template has to stay a plain 404.
    #[tokio::test]
    async fn an_in_use_template_is_a_404_for_anyone_but_its_owner() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let app = router(store.clone());
        let cookie = test_auth_cookie(&app).await;
        let hash = crate::auth::hash_password("bob-pass-123456").unwrap();
        let bob = store.create_user("bob", &hash, "user").await.unwrap();
        let template = store
            .create_template_for_owner(bob.id, seeded_template("bobs-template"))
            .await
            .unwrap();
        let mut task = seeded_task("bobs-task");
        task.template_id = Some(template.id);
        task.url = String::new();
        store.create_for_owner(bob.id, task).await.unwrap();

        // route_admin holds the session, the template belongs to bob.
        let response = delete_template_request(&app, &cookie, template.id).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let (_, _, body) = response_json(response).await;
        assert_eq!(body["code"], "template_not_found");
        // And bob's template survived an attempt from outside.
        assert!(
            store
                .get_template_for_owner(template.id, bob.id)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// Read the run log through its raw query string, so the parsing is exercised
    /// the way a browser exercises it.
    async fn get_runs(app: &Router, cookie: &str, query: &str) -> Value {
        let uri = if query.is_empty() {
            "/api/v1/runs".to_string()
        } else {
            format!("/api/v1/runs?{query}")
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn run_ids(page: &Value) -> Vec<i64> {
        page["items"]
            .as_array()
            .expect("a page of runs")
            .iter()
            .map(|run| run["id"].as_i64().expect("every run has an id"))
            .collect()
    }

    /// The aggregated run log is the one list the *server* pages, so its query
    /// string has to be read as numbers.
    ///
    /// It was read off a `serde_json::Value`, where a query value stays the text
    /// it arrived as: `limit` fell back to the default 100 rows however many the
    /// reader asked for, `task_id` was dropped, and `before_id` was always absent
    /// so the cursor was a no-op and "next page" re-served the first one. Only
    /// `status` behaved, because `as_str` is the reading a text value can satisfy
    /// — which is exactly what made the others look fine.
    #[tokio::test]
    async fn run_log_honours_its_row_count_and_cursor() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let app = router(store.clone());
        let cookie = test_auth_cookie(&app).await;
        let owner = store
            .list_users()
            .await
            .unwrap()
            .into_iter()
            .find(|user| user.username == "route_admin")
            .expect("bootstrap created the owner");
        let task = store
            .create_for_owner(owner.id, seeded_task("paged-logs"))
            .await
            .unwrap();
        let other = store
            .create_for_owner(owner.id, seeded_task("other-logs"))
            .await
            .unwrap();
        // `idx_runs_active_task` allows one active run per task, so each seeded
        // run is closed before the next one opens. One of them fails, so the
        // status filter has something to separate.
        for _ in 0..4 {
            let run = store.start_run(task.id).await.unwrap();
            store.finish_run(run.id, Some(200), None).await.unwrap();
        }
        let failed = store.start_run(task.id).await.unwrap();
        store
            .finish_run(failed.id, None, Some("upstream refused"))
            .await
            .unwrap();
        let run = store.start_run(other.id).await.unwrap();
        store.finish_run(run.id, Some(200), None).await.unwrap();

        // The row count is the reader's, not the default.
        let first = get_runs(&app, &cookie, "limit=2").await;
        assert_eq!(run_ids(&first).len(), 2, "{first}");
        assert_eq!(first["has_more"], json!(true), "{first}");
        let cursor = first["next_cursor"]
            .as_i64()
            .expect("a cursor for the second page");

        // The cursor moves the window instead of repeating it.
        let second = get_runs(&app, &cookie, &format!("limit=2&before_id={cursor}")).await;
        assert_eq!(run_ids(&second).len(), 2, "{second}");
        assert!(run_ids(&second).iter().all(|id| *id < cursor), "{second}");
        let overlap = run_ids(&second)
            .into_iter()
            .filter(|id| run_ids(&first).contains(id))
            .count();
        assert_eq!(overlap, 0, "the second page repeats the first: {second}");

        // `task_id` reaches the filter rather than being dropped on the floor.
        let filtered = get_runs(&app, &cookie, &format!("task_id={}&limit=50", task.id)).await;
        assert_eq!(run_ids(&filtered).len(), 5, "{filtered}");

        // `status` reaches it too — and always did, which is part of the reason
        // the numbers beside it went unnoticed for so long.
        let failures = get_runs(&app, &cookie, "status=failed").await;
        assert_eq!(run_ids(&failures), vec![failed.id], "{failures}");
        assert_eq!(
            run_ids(&get_runs(&app, &cookie, "status=succeeded").await).len(),
            5
        );
        // Filters combine rather than the last one winning.
        let both = get_runs(
            &app,
            &cookie,
            &format!("status=failed&task_id={}", other.id),
        )
        .await;
        assert!(run_ids(&both).is_empty(), "{both}");
        assert!(
            run_ids(&get_runs(&app, &cookie, &format!("status=failed&task_id={}", task.id)).await)
                == vec![failed.id]
        );

        // And the documented default is still "everything, up to a hundred".
        let all = get_runs(&app, &cookie, "").await;
        assert_eq!(run_ids(&all).len(), 6, "{all}");
        assert_eq!(all["has_more"], json!(false), "{all}");
        assert_eq!(all["next_cursor"], Value::Null, "{all}");
    }
}
