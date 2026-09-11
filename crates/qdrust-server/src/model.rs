use serde::{Deserialize, Serialize};

use qdrust_core::plugin::PluginCapability;
use qdrust_core::template::TemplateDefinition;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: String,
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct UserCredentials {
    pub user: User,
    pub password_hash: String,
    pub session_version: i64,
}

#[derive(Clone, Debug)]
pub struct AuthenticatedSession {
    pub user: User,
    pub csrf_token_hash: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct IssuedSession {
    pub session_token: String,
    pub csrf_token: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuthCredentials {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthResponse {
    pub user: User,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateTask {
    pub name: String,
    pub cron: String,
    pub method: Option<String>,
    pub url: String,
    #[serde(default)]
    pub headers: serde_json::Map<String, serde_json::Value>,
    pub body: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    pub template_id: Option<i64>,
    #[serde(default)]
    pub grp: Option<String>,
    /// Per-request timeout in seconds (overrides the global request timeout).
    #[serde(default)]
    pub timeout_seconds: Option<i64>,
    /// Retry count after a failed run: 0 = never, -1 = always, N = up to N retries.
    #[serde(default)]
    pub retry_count: Option<i64>,
    /// Delay before each retry, in seconds (default 60).
    #[serde(default)]
    pub retry_interval_seconds: Option<i64>,
    /// Scheduling priority (higher claims runs first).
    #[serde(default)]
    pub priority: Option<i64>,
    /// IANA timezone name used for cron scheduling (default UTC).
    #[serde(default)]
    pub timezone: Option<String>,
    /// Random delay before a due run executes, in seconds (0 = disabled). The
    /// actual jitter is drawn uniformly from 0..=max at enqueue time.
    #[serde(default)]
    pub random_delay_max_seconds: Option<i64>,
    /// Seed variables rendered into URLs, headers, bodies and template tasks.
    #[serde(default)]
    pub variables: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateTask {
    pub name: Option<String>,
    pub cron: Option<String>,
    pub method: Option<String>,
    pub url: Option<String>,
    pub headers: Option<serde_json::Map<String, serde_json::Value>>,
    pub body: Option<String>,
    pub disabled: Option<bool>,
    pub template_id: Option<i64>,
    pub grp: Option<Option<String>>,
    pub timeout_seconds: Option<Option<i64>>,
    pub retry_count: Option<Option<i64>>,
    pub retry_interval_seconds: Option<Option<i64>>,
    pub priority: Option<Option<i64>>,
    pub timezone: Option<Option<String>>,
    pub random_delay_max_seconds: Option<Option<i64>>,
    pub variables: Option<Option<serde_json::Value>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Task {
    pub id: i64,
    pub name: String,
    pub cron: String,
    pub method: String,
    pub url: String,
    pub headers: serde_json::Value,
    pub body: Option<String>,
    pub disabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_run_at: Option<i64>,
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
    pub template_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_interval_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub random_delay_max_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateTemplate {
    pub name: String,
    pub description: Option<String>,
    pub definition: TemplateDefinition,
    #[serde(default)]
    pub grp: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateTemplate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub definition: Option<TemplateDefinition>,
    pub grp: Option<Option<String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ImportQdHarTemplate {
    pub name: String,
    pub description: Option<String>,
    pub har: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateQdHarTemplate {
    pub name: String,
    pub description: Option<String>,
    pub har: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RegisterUser {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct VerifyEmail {
    pub token: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AdminUserUpdate {
    pub disabled: Option<bool>,
    pub role: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ForgotPassword {
    pub username: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResetPassword {
    pub token: String,
    pub new_password: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SiteSetting {
    pub key: String,
    pub value: serde_json::Value,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SetSiteSetting {
    pub value: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BatchTaskOperation {
    pub ids: Vec<i64>,
    #[serde(default)]
    pub action: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ClearLogs {
    #[serde(default)]
    pub older_than_days: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ValidateQdHar {
    pub har: serde_json::Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct BatchTaskResult {
    pub updated: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct QdHarValidation {
    pub valid: bool,
    pub entries: usize,
    pub enabled: usize,
    pub requests: usize,
    pub controls: usize,
    pub extract_variables: usize,
}

// ---- P1 features: template subscriptions, push requests, email verification ----

#[derive(Clone, Debug, Serialize)]
pub struct TemplateSubscription {
    pub id: i64,
    pub owner_id: i64,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub last_synced_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateTemplateSubscription {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateTemplateSubscription {
    pub name: Option<String>,
    pub url: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SubscriptionSync {
    pub id: i64,
    pub subscription_id: i64,
    pub status: String,
    pub message: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PushRequest {
    pub id: i64,
    pub owner_id: i64,
    pub template_id: i64,
    pub status: String,
    pub note: Option<String>,
    pub reviewed_by: Option<i64>,
    pub reviewed_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreatePushRequest {
    pub template_id: i64,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DecidePushRequest {
    pub approve: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Template {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub schema_version: i64,
    pub source_format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<TemplateDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qd_har: Option<serde_json::Value>,
    /// Input variables a task created from this template has to supply, in
    /// first-appearance order: for QD HARs the `{{name}}` reads QD lists as its
    /// "变量" form, for native templates the declared `variables` map keys.
    #[serde(default)]
    pub variables: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grp: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Run {
    pub id: i64,
    pub task_id: i64,
    pub status: String,
    pub http_status: Option<i64>,
    pub error: Option<String>,
    /// QD-style log line: the final __log__ value extracted by the template
    /// execution (empty for plain tasks and failed runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub created_at: i64,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub attempt: i64,
    pub cancel_requested: bool,
    /// Earliest claim time for delayed (retry) runs; NULL means immediately claimable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_after: Option<i64>,
    /// For retry runs: id of the first (original) run of the retry chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<i64>,
    pub trigger: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunStep {
    pub id: i64,
    pub run_id: i64,
    pub step_index: i64,
    pub name: String,
    pub status: String,
    pub http_status: Option<i64>,
    pub body_size: i64,
    pub error: Option<String>,
    pub started_at: i64,
    pub finished_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationChannel {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateNotificationChannel {
    pub name: String,
    pub kind: String,
    pub config: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateNotificationChannel {
    pub name: Option<String>,
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationAction {
    pub id: i64,
    pub task_id: i64,
    pub channel_id: i64,
    pub event: String,
    pub failure_threshold: i64,
    pub automatic_only: bool,
    pub title_template: Option<String>,
    pub body_template: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateNotificationAction {
    pub channel_id: i64,
    pub event: String,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: i64,
    #[serde(default)]
    pub automatic_only: bool,
    #[serde(default)]
    pub title_template: Option<String>,
    #[serde(default)]
    pub body_template: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BatchCreateNotificationAction {
    pub task_ids: Vec<i64>,
    #[serde(flatten)]
    pub action: CreateNotificationAction,
}

#[derive(Clone, Debug)]
pub struct NotificationDelivery {
    pub channel: NotificationChannel,
    pub action: NotificationAction,
}

fn default_failure_threshold() -> i64 {
    1
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginManifest {
    pub id: i64,
    pub name: String,
    pub command: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreatePluginManifest {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdatePluginManifest {
    pub name: Option<String>,
    pub command: Option<String>,
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InvokePlugin {
    pub action: String,
    #[serde(default)]
    pub query: std::collections::BTreeMap<String, String>,
}

/// Capabilities a plugin declares in its `config` JSON
/// (`{"capabilities": ["network", ...]}`). A missing key means no capabilities
/// are declared. Unknown names are errors so a typo fails at save time instead
/// of silently degrading to "nothing declared", which would make every
/// capability-reporting call fail at run time.
pub fn plugin_capabilities(config: &serde_json::Value) -> Result<Vec<PluginCapability>, String> {
    match config.get("capabilities") {
        None => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|err| format!("invalid plugin capabilities: {err}")),
    }
}

/// A link between an external identity (provider + issuer + subject) and a
/// local `users` row. See docs/EXTERNAL_IDP_PLAN.md §3.2.
#[derive(Clone, Debug)]
pub struct ExternalIdentity {
    pub id: i64,
    pub user_id: i64,
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub created_at: i64,
    pub last_login_at: i64,
}

/// The resolved, provider-independent identity handed to the shared
/// "external login" layer. Business handlers never see provider internals.
#[derive(Clone, Debug)]
pub struct ExternalIdentityClaim {
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub username_hint: Option<String>,
    pub groups: Vec<String>,
}

/// Result of the external-identity -> local-user resolution step.
#[derive(Clone, Debug)]
pub struct ExternalLoginResolution {
    /// The local user matched or provisioned, if login should proceed.
    pub user: Option<User>,
    /// True when this call created a brand-new external user row.
    pub created: bool,
    /// A machine-readable reason when `user` is None due to a conflict or
    /// policy refusal (e.g. `"external_identity_conflict"`, `"user_disabled"`,
    /// `"provisioning_disabled"`). Populated only on refusal.
    pub refusal: Option<String>,
}

/// One row of the `oidc_login_states` table (DB fallback for the OIDC
/// authorization state; Redis is preferred when configured).
#[derive(Clone, Debug)]
pub struct OidcLoginState {
    pub state_hash: String,
    pub nonce_hash: String,
    pub pkce_verifier_encrypted: String,
    pub redirect_uri: String,
    pub created_at: i64,
    pub expires_at: i64,
}
