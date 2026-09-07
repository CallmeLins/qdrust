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
use qdrust_core::plugin::{
    PLUGIN_API_VERSION, Plugin, PluginManifest as CorePluginManifest, PluginRequest,
    SubprocessPlugin,
};
use qdrust_core::qd_har::{QdHar, QdProgram};
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
        AdminUserUpdate, AuthCredentials, AuthResponse, AuthenticatedSession, BatchTaskOperation,
        BatchTaskResult, ChangePassword, ClearLogs, CreateNotificationAction,
        CreateNotificationChannel, CreatePluginManifest, CreatePushRequest, CreateTask,
        CreateTemplate, CreateTemplateSubscription, DecidePushRequest, ExternalIdentityClaim,
        ForgotPassword, ImportQdHarTemplate, InvokePlugin, IssuedSession, QdHarValidation,
        RegisterUser, ResetPassword, SetSiteSetting, UpdateNotificationChannel,
        UpdatePluginManifest, UpdateQdHarTemplate, UpdateTask, UpdateTemplate,
        UpdateTemplateSubscription, ValidateQdHar, VerifyEmail,
    },
    store::Store,
};
use openidconnect::{AuthorizationCode, Nonce, PkceCodeVerifier};

const SESSION_COOKIE: &str = "qd_session";
const CSRF_COOKIE: &str = "qd_csrf";

/// Runtime-tunable settings that can be updated without a restart.
#[derive(Clone, Debug, Default)]
pub struct RuntimeSettings {
    pub require_email_verification: bool,
    pub ga_key: Option<String>,
    pub log_retention_days: u64,
}

pub fn runtime_settings() -> std::sync::Arc<std::sync::RwLock<RuntimeSettings>> {
    std::sync::Arc::new(std::sync::RwLock::new(RuntimeSettings::default()))
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
            },
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
    subscription_events: broadcast::Sender<Value>,
    settings: std::sync::Arc<std::sync::RwLock<RuntimeSettings>>,
    http_client: reqwest::Client,
    session_cache: crate::redis_cache::SessionCache,
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
    let (subscription_events, _) = subscription_event_channel();
    router_with_auth(
        store,
        AuthConfig::default(),
        run_events,
        subscription_events,
        runtime_settings(),
        reqwest::Client::new(),
        crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
        "",
        None,
    )
}

pub fn subscription_event_channel() -> (broadcast::Sender<Value>, broadcast::Receiver<Value>) {
    broadcast::channel(256)
}

#[allow(clippy::too_many_arguments)]
pub fn router_with_auth(
    store: Store,
    auth: AuthConfig,
    run_events: RunEventSender,
    subscription_events: broadcast::Sender<Value>,
    settings: std::sync::Arc<std::sync::RwLock<RuntimeSettings>>,
    http_client: reqwest::Client,
    session_cache: crate::redis_cache::SessionCache,
    base_path: &str,
    header_auth: Option<std::sync::Arc<crate::config::HeaderAuthConfig>>,
) -> Router {
    let login_limiter =
        LoginRateLimiter::new(auth.login_rate_limit_attempts, auth.login_rate_limit_window)
            .expect("login rate limit configuration must be valid");
    let inner = Router::new()
        .route("/api/v1/openapi.json", get(openapi))
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
        .route("/api/v1/runs", delete(delete_all_runs))
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
            "/api/v1/subscriptions/{id}/sync",
            axum::routing::post(sync_subscription_now),
        )
        .route(
            "/api/v1/subscriptions/{id}/syncs",
            get(list_subscription_syncs),
        )
        .route(
            "/api/v1/subscriptions/{id}/sync/live",
            get(subscription_sync_websocket),
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
            "/api/v1/tasks/{id}/notification-actions",
            get(list_notification_actions).post(create_notification_action),
        )
        .route(
            "/api/v1/notification-actions/{id}",
            axum::routing::delete(delete_notification_action),
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
        subscription_events,
        settings,
        http_client,
        session_cache,
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
    let email = claims.email().map(|e| e.as_str().to_string());
    let username_hint = claims.preferred_username().map(|u| u.as_str().to_string());

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
/// existing user being reused. See EXTERNAL_IDP_PLAN.md Phase 2.
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
            "Initial administrator already exists",
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
/// short-circuits with 403 and an audit entry. See EXTERNAL_IDP_PLAN.md Phase 4.
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
        "Task already has an active run",
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
        Err(ApiError::Conflict("run_not_active", "Run is not active"))
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
    const CHANNEL_KINDS: [&str; 10] = [
        "webhook",
        "email",
        "bark",
        "serverchan",
        "telegram",
        "dingtalk",
        "wxpusher",
        "wxpusher_spt",
        "wecom_app",
        "wecom_webhook",
    ];
    if !CHANNEL_KINDS.contains(&input.kind.as_str()) {
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

async fn list_templates(
    State(store): State<Store>,
    headers: HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    let query = params.get("q").and_then(|v| v.as_str());
    let grp = params.get("grp").and_then(|v| v.as_str());
    let cursor = params.get("cursor").and_then(|v| v.as_i64());
    let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
    let templates = store
        .search_templates_for_owner(session.user.id, query, grp, cursor, limit)
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

async fn delete_template(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    if store.delete_template_for_owner(id, session.user.id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(
            "template_not_found",
            "Template not found",
        ))
    }
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
                "Username is already registered or invalid",
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

async fn sync_subscription_now(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    let subscription = state
        .store
        .get_subscription(id, session.user.id)
        .await?
        .ok_or(ApiError::NotFound(
            "subscription_not_found",
            "Subscription not found",
        ))?;
    let store = state.store.clone();
    let client = state.http_client.clone();
    let events = state.subscription_events.clone();
    tokio::spawn(async move {
        match crate::subscriptions::sync_subscription(&store, &client, &subscription, Some(events))
            .await
        {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(%err, "subscription sync failed");
            }
        }
    });
    Ok(Json(json!({"status": "started"})))
}

async fn list_subscription_syncs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    state
        .store
        .list_subscription_syncs(id, session.user.id)
        .await?
        .map(|syncs| Json(json!(syncs)))
        .ok_or(ApiError::NotFound(
            "subscription_not_found",
            "Subscription not found",
        ))
}

async fn subscription_sync_websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let (_, session) = require_session(&state, &headers).await?;
    if state
        .store
        .get_subscription(id, session.user.id)
        .await?
        .is_none()
    {
        return Err(ApiError::NotFound(
            "subscription_not_found",
            "Subscription not found",
        ));
    }
    let events = state.subscription_events.subscribe();
    Ok(ws.on_upgrade(move |socket| stream_subscription_events(socket, id, events)))
}

async fn stream_subscription_events(
    mut socket: axum::extract::ws::WebSocket,
    subscription_id: i64,
    mut events: tokio::sync::broadcast::Receiver<Value>,
) {
    use futures_util::SinkExt;
    loop {
        let event = tokio::select! {
            _ = socket.recv() => break,
            event = events.recv() => match event {
                Ok(value) => value,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            },
        };
        if event.get("subscription_id").and_then(|v| v.as_i64()) != Some(subscription_id) {
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
    Conflict(&'static str, &'static str),
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
            Self::Conflict(code, message) => (StatusCode::CONFLICT, code, message.into()),
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
            subscription_event_channel().0,
            runtime_settings(),
            reqwest::Client::new(),
            crate::redis_cache::SessionCache::from_env().expect("invalid REDIS_URL"),
            base_path,
            None,
        )
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
            subscription_event_channel().0,
            runtime_settings(),
            reqwest::Client::new(),
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
        // No secret material may ever appear in this response.
        assert!(!text.to_lowercase().contains("secret"));
        assert!(!text.to_lowercase().contains("client_id"));
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
        assert!(document["paths"]["/api/v1/tasks"].is_object());
        assert_eq!(
            document["components"]["schemas"]["ApiError"]["required"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
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

    // --- External identity login audit trail (EXTERNAL_IDP_PLAN.md Phase 2) ---

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
            subscription_event_channel().0,
            runtime_settings(),
            reqwest::Client::new(),
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
                subscription_event_channel().0,
                runtime_settings(),
                reqwest::Client::new(),
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
}
