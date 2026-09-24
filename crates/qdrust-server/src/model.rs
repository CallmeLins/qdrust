use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Request URL for a task that is not bound to a template. It may be omitted
    /// when `template_id` is set: the server then mirrors the template's first
    /// request, because a bound task always executes the template.
    #[serde(default)]
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

/// Body of `POST /api/v1/templates/{id}/test`: the variable values to run the
/// template with. Anything omitted renders as undefined, exactly as a task that
/// never filled that variable would.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TestTemplate {
    #[serde(default)]
    pub variables: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TemplateTestStep {
    pub index: usize,
    pub url: String,
    pub status: u16,
    pub body_size: usize,
}

/// A test run's outcome. Nothing is persisted: the point is to answer "does
/// this template work with these variables" before a task is saved.
#[derive(Clone, Debug, Serialize)]
pub struct TemplateTestResult {
    pub steps: Vec<TemplateTestStep>,
    pub variables: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

// ---- P1 features: template subscriptions, push requests, email verification ----

/// A source the user browses and imports templates from on demand. There is no
/// import mode: a subscription is a library, never something that imports on
/// its own. See [`crate::library`].
#[derive(Clone, Debug, Serialize)]
pub struct TemplateSubscription {
    pub id: i64,
    pub owner_id: i64,
    pub name: String,
    pub url: String,
    pub enabled: bool,
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

/// One template offered by a subscription source, as seen by the library
/// browser. `installed*` / `update_available` are resolved against the local
/// templates the subscription already imported.
#[derive(Clone, Debug, Serialize)]
pub struct LibraryEntry {
    /// The entry's identity inside the source. For a manifest source this is
    /// the `har` key; for a scanned repository it is the file path.
    pub name: String,
    pub author: Option<String>,
    pub comments: Option<String>,
    /// Upstream revision (the manifest's `yyyymmdd` version). Absent for
    /// sources that publish no manifest.
    pub version: Option<String>,
    pub date: Option<String>,
    pub filename: String,
    pub url: Option<String>,
    pub comment_url: Option<String>,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_template_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// True when the entry is installed at an older version than the source
    /// now offers.
    pub update_available: bool,
    /// Which source the entry came from. Only the aggregate listing fills these
    /// in — a single-source listing already knows, and its rows act against
    /// that source — so they stay absent there rather than repeating it on
    /// every row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_name: Option<String>,
}

/// A subscription source's catalogue.
#[derive(Clone, Debug, Serialize)]
pub struct TemplateLibrary {
    pub subscription_id: i64,
    /// `manifest` when the source publishes `tpls_history.json`, `files` when
    /// the catalogue had to be derived by scanning the repository tree.
    pub source_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
    pub entries: Vec<LibraryEntry>,
}

/// Every subscribed source's catalogue in one list, which is what a QD user
/// means by "public templates": one page of everything on offer, not one page
/// per repository.
#[derive(Clone, Debug, Serialize)]
pub struct LibraryOverview {
    /// Entries from every source that could be read, in source order.
    pub entries: Vec<LibraryEntry>,
    /// How each source contributed, including the ones that failed. A single
    /// unreachable repository must not blank the whole page, so its error is
    /// reported here instead of failing the request.
    pub sources: Vec<LibrarySourceStatus>,
    /// True when the total entry cap was hit, so the list is short of what the
    /// sources actually offer.
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LibrarySourceStatus {
    pub subscription_id: i64,
    pub name: String,
    /// `manifest` or `files`; absent when the source could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    /// Entries this source contributed to the response, after the cap.
    pub entries: usize,
    /// True when the catalogue was reused instead of fetched again.
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One entry fetched for inspection but deliberately not written yet.
///
/// This is the difference that keeps subscribing from littering the template
/// list: the user sees the upstream template, edits it, and only a save writes
/// anything.
#[derive(Clone, Debug, Serialize)]
pub struct LibraryPreview {
    /// The entry's metadata resolved against local state, exactly as the
    /// listing shows it.
    pub entry: LibraryEntry,
    /// The upstream HAR document, untouched.
    pub har: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ImportLibraryTemplates {
    /// Entry names to import, as returned by the library listing.
    pub names: Vec<String>,
}

/// Save the template a user edited out of a preview.
#[derive(Clone, Debug, Deserialize)]
pub struct ApplyLibraryTemplate {
    /// Entry identity inside the source, as returned by the listing.
    pub entry: String,
    /// Local template to refresh in place. Absent creates one, falling back to
    /// a name match so an entry that was imported before provenance existed is
    /// refreshed rather than duplicated.
    #[serde(default)]
    pub template_id: Option<i64>,
    /// Name to save under. The editor allows changing it, so this can differ
    /// from the entry's own name.
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The HAR document as edited in the browser.
    pub har: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct LibraryImportResult {
    pub imported: usize,
    pub updated: usize,
    /// Entries that could not be imported. Reported per entry rather than
    /// aborting the batch, so one broken upstream template does not block the
    /// rest of the selection.
    pub failed: Vec<LibraryImportFailure>,
    /// Every entry this call wrote, in the order it was requested. Counters
    /// alone left the caller unable to act on what it just pulled in, so the
    /// id travels with the name (open the editor prefilled, build a task).
    pub templates: Vec<LibraryImportOutcome>,
}

/// One entry that landed in the store during an import.
#[derive(Clone, Debug, Serialize)]
pub struct LibraryImportOutcome {
    pub name: String,
    pub template_id: i64,
    /// `true` when an existing template was refreshed in place rather than
    /// created.
    pub updated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LibraryImportFailure {
    pub name: String,
    pub error: String,
}

/// Provenance row linking a local template back to the source entry it came
/// from, used to mark entries as installed and to detect upstream updates.
#[derive(Clone, Debug, Serialize)]
pub struct TemplateImport {
    pub subscription_id: i64,
    pub template_id: i64,
    pub entry_name: String,
    pub entry_version: Option<String>,
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
    /// Default values for those variables, keyed by name: QD's `init_env`
    /// (`{{name|default("...")}}`) for HAR templates, the declared `variables`
    /// map for native ones. The new-task form seeds each row with this instead
    /// of an empty box.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub variable_defaults: BTreeMap<String, String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// How many of the owner's tasks are bound to this template right now.
    /// Counted on read (see `TEMPLATE_FIELDS`) rather than stored, so it can
    /// never drift from the `tasks` table: "which templates are still unused"
    /// is the same question the create-task dropdown asks, and both sides now
    /// read one number instead of each keeping its own list.
    #[serde(default)]
    pub task_count: i64,
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

/// Partial update for one task↔channel binding.
///
/// Every field is optional and an absent field keeps its stored value. The two
/// templates are plain `Option<String>` rather than `Option<Option<String>>`
/// because JSON cannot tell "absent" from `null` here; sending an empty (or
/// whitespace-only) string clears a template instead.
#[derive(Clone, Debug, Deserialize)]
pub struct UpdateNotificationAction {
    pub channel_id: Option<i64>,
    pub event: Option<String>,
    pub failure_threshold: Option<i64>,
    pub automatic_only: Option<bool>,
    pub title_template: Option<String>,
    pub body_template: Option<String>,
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
/// local `users` row. See docs/design/EXTERNAL_IDP_PLAN.md §3.2.
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
