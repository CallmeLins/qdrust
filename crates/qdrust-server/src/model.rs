use serde::{Deserialize, Serialize};

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
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub created_at: i64,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub attempt: i64,
    pub cancel_requested: bool,
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
pub struct Note {
    pub id: i64,
    pub title: String,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateNote {
    pub title: String,
    #[serde(default)]
    pub content: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UpdateNote {
    pub title: Option<String>,
    pub content: Option<String>,
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
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateNotificationAction {
    pub channel_id: i64,
    pub event: String,
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
