use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        FromRef, FromRequest, Path, Request as AxumRequest, State, rejection::JsonRejection,
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{COOKIE, HeaderName, SET_COOKIE},
    },
    response::{IntoResponse, Response},
    routing::{any, get},
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
use tower_http::services::{ServeDir, ServeFile};

use crate::auth::LoginRateLimiter;
use crate::auth::{hash_password, token_hash, verify_password};
use crate::{
    model::{
        AuthCredentials, AuthResponse, AuthenticatedSession, ChangePassword, CreateNote,
        CreateNotificationAction, CreateNotificationChannel, CreatePluginManifest, CreateTask,
        CreateTemplate, ImportQdHarTemplate, InvokePlugin, IssuedSession, QdHarValidation,
        UpdateNote, UpdateNotificationChannel, UpdatePluginManifest, UpdateQdHarTemplate,
        UpdateTask, UpdateTemplate, ValidateQdHar,
    },
    store::Store,
};

const SESSION_COOKIE: &str = "qd_session";
const CSRF_COOKIE: &str = "qd_csrf";

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub session_ttl: Duration,
    pub cookie_secure: bool,
    pub login_rate_limit_attempts: u32,
    pub login_rate_limit_window: Duration,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_ttl: Duration::from_secs(604_800),
            cookie_secure: false,
            login_rate_limit_attempts: 5,
            login_rate_limit_window: Duration::from_secs(60),
        }
    }
}

#[derive(Clone)]
struct AppState {
    store: Store,
    auth: AuthConfig,
    login_limiter: LoginRateLimiter,
}

impl FromRef<AppState> for Store {
    fn from_ref(state: &AppState) -> Self {
        state.store.clone()
    }
}

pub fn router(store: Store) -> Router {
    router_with_auth(store, AuthConfig::default())
}

pub fn router_with_auth(store: Store, auth: AuthConfig) -> Router {
    let login_limiter =
        LoginRateLimiter::new(auth.login_rate_limit_attempts, auth.login_rate_limit_window)
            .expect("login rate limit configuration must be valid");
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/api/v1/openapi.json", get(openapi))
        .route("/api/v1/auth/bootstrap", axum::routing::post(bootstrap))
        .route("/api/v1/auth/login", axum::routing::post(login))
        .route("/api/v1/auth/session", get(current_session))
        .route("/api/v1/auth/logout", axum::routing::post(logout))
        .route(
            "/api/v1/auth/password",
            axum::routing::post(change_password),
        )
        .route("/api/v1/tasks", get(list_tasks).post(create_task))
        .route(
            "/api/v1/tasks/{id}",
            get(get_task).put(update_task).delete(delete_task),
        )
        .route("/api/v1/tasks/{id}/runs", get(list_task_runs))
        .route("/api/v1/tasks/{id}/run", axum::routing::post(run_task))
        .route("/api/v1/runs/{id}/cancel", axum::routing::post(cancel_run))
        .route("/api/v1/runs/{id}/steps", get(list_run_steps))
        .route("/api/v1/notes", get(list_notes).post(create_note))
        .route(
            "/api/v1/notes/{id}",
            get(get_note).put(update_note).delete(delete_note),
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
        .fallback_service(
            ServeDir::new("webui/dist").not_found_service(ServeFile::new("webui/dist/index.html")),
        )
        .with_state(AppState {
            store,
            auth,
            login_limiter,
        })
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

async fn login(
    State(state): State<AppState>,
    ApiJson(input): ApiJson<AuthCredentials>,
) -> Result<Response, ApiError> {
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
    require_session_from_store(&state.store, headers).await
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
async fn list_tasks(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok(Json(json!(store.list_for_owner(session.user.id).await?)))
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

async fn list_notes(
    State(store): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok(Json(json!(store.list_notes(session.user.id).await?)))
}
async fn create_note(
    State(store): State<Store>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<CreateNote>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!(
            store
                .create_note(session.user.id, input)
                .await
                .map_err(ApiError::unprocessable)?
        )),
    ))
}
async fn get_note(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .get_note(id, session.user.id)
        .await?
        .map(|n| Json(json!(n)))
        .ok_or(ApiError::NotFound("note_not_found", "Note not found"))
}
async fn update_note(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    ApiJson(input): ApiJson<UpdateNote>,
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    store
        .update_note(id, session.user.id, input)
        .await
        .map_err(ApiError::unprocessable)?
        .map(|n| Json(json!(n)))
        .ok_or(ApiError::NotFound("note_not_found", "Note not found"))
}
async fn delete_note(
    State(store): State<Store>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    if store.delete_note(id, session.user.id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("note_not_found", "Note not found"))
    }
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
) -> Result<Json<Value>, ApiError> {
    let (_, session) = require_session_from_store(&store, &headers).await?;
    Ok(Json(json!(
        store.list_templates_for_owner(session.user.id).await?
    )))
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
    let runner = SubprocessPlugin::new(
        CorePluginManifest {
            api_version: PLUGIN_API_VERSION,
            id: format!("plugin-{id}"),
            name: plugin.name.clone(),
            version: "1".into(),
            capabilities: vec![],
        },
        &plugin.command,
        vec![],
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
    let har = QdHar::parse(input.har).map_err(ApiError::unprocessable)?;
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
        http::Request,
    };
    use tower::ServiceExt;

    use super::*;

    async fn test_app() -> Router {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        router(store)
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
}
