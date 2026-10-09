//! `qdrust-mcp` — a stdio MCP server that exposes a qdrust instance's REST API
//! to MCP clients (Claude Desktop, Cursor, …), so a user can create, run and
//! inspect tasks without leaving the client.
//!
//! Configuration is environment-only, because an MCP client spawns this binary
//! and passes the environment:
//!
//! - `QDRUST_URL`   — base URL of the qdrust server. Default `http://localhost:8923`.
//! - `QDRUST_TOKEN` — a personal access token (`qd_…`) created in
//!   **Settings → API tokens**. Required.
//!
//! The token carries its owner's full API access, so treat it like a password.
//! Every call is made with `Authorization: Bearer <token>`, which authenticates
//! as that user and skips CSRF (there is no cookie to protect).
//!
//! Typical client configuration:
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "qdrust": {
//!       "command": "qdrust-mcp",
//!       "env": {
//!         "QDRUST_URL": "https://qd.example.com",
//!         "QDRUST_TOKEN": "qd_..."
//!       }
//!     }
//!   }
//! }
//! ```

use std::future::Future;

use anyhow::{Context, Result, anyhow, bail};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::ErrorData,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// A thin, authenticated REST client for one qdrust instance.
#[derive(Clone)]
struct QdrustClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl QdrustClient {
    fn new(base: &str, token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("cannot build the HTTP client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            http,
        })
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut request = self
            .http
            .request(method, &url)
            .bearer_auth(&self.token)
            .header("accept", "application/json");
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("request to {url} failed"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("qdrust returned {status}: {}", truncate(&text, 600));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .with_context(|| format!("qdrust returned non-JSON: {}", truncate(&text, 200)))
    }

    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.request(reqwest::Method::GET, path, query, None).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.request(reqwest::Method::POST, path, &[], Some(body))
            .await
    }

    async fn put(&self, path: &str, body: Value) -> Result<Value> {
        self.request(reqwest::Method::PUT, path, &[], Some(body))
            .await
    }

    async fn delete(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::DELETE, path, &[], None).await
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// JSON for a tool result, or an MCP tool error with the server's message.
async fn run(future: impl Future<Output = Result<Value>>) -> Result<String, ErrorData> {
    match future.await {
        Ok(value) => Ok(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "null".into())),
        Err(err) => Err(ErrorData::internal_error(format!("{err:#}"), None)),
    }
}

/// Build an update body from the fields the caller actually set. An omitted
/// field keeps its stored value on the server, so sending `null` is not the
/// same thing and must never happen by accident.
fn object_from(entries: impl IntoIterator<Item = (&'static str, Option<Value>)>) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        if let Some(value) = value {
            map.insert(key.to_owned(), value);
        }
    }
    Value::Object(map)
}

// ---------------------------------------------------------------- parameters

#[derive(Debug, Deserialize, JsonSchema)]
struct ListTasksParams {
    /// Only tasks in this group.
    grp: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IdParams {
    /// Numeric resource id.
    id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateTaskParams {
    /// Task name.
    name: String,
    /// Cron schedule. qdrust uses the 7-field form `sec min hour day month weekday year`
    /// (year may be `*`); for example `0 0 8 * * * *` is 08:00 every day.
    cron: String,
    /// Template to bind. A bound task replays the template, so `url` is ignored.
    template_id: Option<i64>,
    /// Group label.
    grp: Option<String>,
    /// IANA timezone the cron is evaluated in (e.g. `Asia/Shanghai`).
    timezone: Option<String>,
    /// Create the task paused.
    disabled: Option<bool>,
    /// Request URL, for a task that is not bound to a template.
    url: Option<String>,
    /// Per-request timeout in seconds.
    timeout_seconds: Option<i64>,
    /// Seed variables rendered into the template (`{"username": "..."}`).
    variables: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UpdateTaskParams {
    /// Task id to update.
    id: i64,
    /// New name.
    name: Option<String>,
    /// New cron schedule.
    cron: Option<String>,
    /// Enable or pause.
    disabled: Option<bool>,
    /// Move to a group; empty string clears it.
    grp: Option<String>,
    /// IANA timezone.
    timezone: Option<String>,
    /// Seed variables (`{"username": "..."}`).
    variables: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListRunsParams {
    /// Filter by status: `pending`, `leased`, `running`, `succeeded`, `failed`, `cancelled`.
    status: Option<String>,
    /// Only runs of this task.
    task_id: Option<i64>,
    /// Page size (1..=500, default 100).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BatchTaskParams {
    /// Task ids to act on.
    ids: Vec<i64>,
    /// One of `enable`, `disable`, `delete`, `run`.
    action: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListTemplatesParams {
    /// Substring match on the template name.
    q: Option<String>,
    /// Page size (1..=200).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TestTemplateParams {
    /// Template id.
    id: i64,
    /// Variable values to run with (`{"username": "..."}`).
    variables: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ImportTemplateParams {
    /// Name for the imported template.
    name: String,
    /// The QD HAR document (`{ "log": { "version": "1.2", "entries": [...] } }`)
    /// or a QD request array.
    har: Value,
    /// Optional description.
    description: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BindNotificationParams {
    /// Tasks to bind.
    task_ids: Vec<i64>,
    /// Notification channel id (see `list_notification_channels`).
    channel_id: i64,
    /// When to send: `success`, `failure` or `always`. Defaults to `failure`.
    event: Option<String>,
}

// -------------------------------------------------------------------- server

#[derive(Clone)]
struct QdrustMcp {
    client: QdrustClient,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl QdrustMcp {
    fn new(client: QdrustClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "List the caller's tasks, newest first.")]
    async fn list_tasks(
        &self,
        Parameters(p): Parameters<ListTasksParams>,
    ) -> Result<String, ErrorData> {
        let query: Vec<(&str, String)> = p.grp.map(|grp| ("grp", grp)).into_iter().collect();
        run(self.client.get("/api/v1/tasks", &query)).await
    }

    #[tool(description = "Fetch one task by id.")]
    async fn get_task(&self, Parameters(p): Parameters<IdParams>) -> Result<String, ErrorData> {
        run(self.client.get(&format!("/api/v1/tasks/{}", p.id), &[])).await
    }

    #[tool(description = "Create a scheduled task. A task's requests come from its template.")]
    async fn create_task(
        &self,
        Parameters(p): Parameters<CreateTaskParams>,
    ) -> Result<String, ErrorData> {
        let body = object_from([
            ("name", Some(json!(p.name))),
            ("cron", Some(json!(p.cron))),
            ("template_id", p.template_id.map(|id| json!(id))),
            ("grp", p.grp.map(|grp| json!(grp))),
            ("timezone", p.timezone.map(|tz| json!(tz))),
            ("disabled", p.disabled.map(|disabled| json!(disabled))),
            ("url", p.url.map(|url| json!(url))),
            ("timeout_seconds", p.timeout_seconds.map(|v| json!(v))),
            ("variables", p.variables),
        ]);
        run(self.client.post("/api/v1/tasks", body)).await
    }

    #[tool(description = "Update a task. Fields left out keep their stored value.")]
    async fn update_task(
        &self,
        Parameters(p): Parameters<UpdateTaskParams>,
    ) -> Result<String, ErrorData> {
        let body = object_from([
            ("name", p.name.map(|name| json!(name))),
            ("cron", p.cron.map(|cron| json!(cron))),
            ("disabled", p.disabled.map(|disabled| json!(disabled))),
            ("grp", p.grp.map(|grp| json!(grp))),
            ("timezone", p.timezone.map(|tz| json!(tz))),
            ("variables", p.variables),
        ]);
        run(self.client.put(&format!("/api/v1/tasks/{}", p.id), body)).await
    }

    #[tool(description = "Delete a task and its runs.")]
    async fn delete_task(&self, Parameters(p): Parameters<IdParams>) -> Result<String, ErrorData> {
        run(self.client.delete(&format!("/api/v1/tasks/{}", p.id))).await
    }

    #[tool(description = "Run a task immediately and return its new run record.")]
    async fn run_task(&self, Parameters(p): Parameters<IdParams>) -> Result<String, ErrorData> {
        run(self
            .client
            .post(&format!("/api/v1/tasks/{}/run", p.id), json!({})))
        .await
    }

    #[tool(description = "Cancel an active run.")]
    async fn cancel_run(&self, Parameters(p): Parameters<IdParams>) -> Result<String, ErrorData> {
        run(self
            .client
            .post(&format!("/api/v1/runs/{}/cancel", p.id), json!({})))
        .await
    }

    #[tool(description = "List runs across the caller's tasks, newest first.")]
    async fn list_runs(
        &self,
        Parameters(p): Parameters<ListRunsParams>,
    ) -> Result<String, ErrorData> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(status) = p.status {
            query.push(("status", status));
        }
        if let Some(task_id) = p.task_id {
            query.push(("task_id", task_id.to_string()));
        }
        if let Some(limit) = p.limit {
            query.push(("limit", limit.to_string()));
        }
        run(self.client.get("/api/v1/runs", &query)).await
    }

    #[tool(description = "List one task's runs.")]
    async fn list_task_runs(
        &self,
        Parameters(p): Parameters<IdParams>,
    ) -> Result<String, ErrorData> {
        run(self
            .client
            .get(&format!("/api/v1/tasks/{}/runs", p.id), &[]))
        .await
    }

    #[tool(description = "List the steps of one run: per-request status and body size.")]
    async fn get_run_steps(
        &self,
        Parameters(p): Parameters<IdParams>,
    ) -> Result<String, ErrorData> {
        run(self
            .client
            .get(&format!("/api/v1/runs/{}/steps", p.id), &[]))
        .await
    }

    #[tool(description = "Enable, pause, delete or run several tasks at once.")]
    async fn batch_tasks(
        &self,
        Parameters(p): Parameters<BatchTaskParams>,
    ) -> Result<String, ErrorData> {
        run(self.client.post(
            "/api/v1/tasks/batch",
            json!({"ids": p.ids, "action": p.action}),
        ))
        .await
    }

    #[tool(description = "List the caller's templates, newest first.")]
    async fn list_templates(
        &self,
        Parameters(p): Parameters<ListTemplatesParams>,
    ) -> Result<String, ErrorData> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(q) = p.q {
            query.push(("q", q));
        }
        if let Some(limit) = p.limit {
            query.push(("limit", limit.to_string()));
        }
        run(self.client.get("/api/v1/templates", &query)).await
    }

    #[tool(description = "Fetch one template by id, including its variable list and defaults.")]
    async fn get_template(&self, Parameters(p): Parameters<IdParams>) -> Result<String, ErrorData> {
        run(self.client.get(&format!("/api/v1/templates/{}", p.id), &[])).await
    }

    #[tool(
        description = "Run a saved template with the given variables and return its steps. Does not create a task or a run."
    )]
    async fn test_template(
        &self,
        Parameters(p): Parameters<TestTemplateParams>,
    ) -> Result<String, ErrorData> {
        run(self.client.post(
            &format!("/api/v1/templates/{}/test", p.id),
            json!({"variables": p.variables.unwrap_or_else(|| json!({}))}),
        ))
        .await
    }

    #[tool(description = "Import a QD HAR document as a template.")]
    async fn import_template(
        &self,
        Parameters(p): Parameters<ImportTemplateParams>,
    ) -> Result<String, ErrorData> {
        run(self.client.post(
            "/api/v1/templates/import-qd-har",
            json!({"name": p.name, "har": p.har, "description": p.description}),
        ))
        .await
    }

    #[tool(description = "List notification channels (Webhook, Email, Telegram, …).")]
    async fn list_notification_channels(&self) -> Result<String, ErrorData> {
        run(self.client.get("/api/v1/notification-channels", &[])).await
    }

    #[tool(description = "Bind a notification channel to one or more tasks.")]
    async fn bind_notification(
        &self,
        Parameters(p): Parameters<BindNotificationParams>,
    ) -> Result<String, ErrorData> {
        run(self.client.post(
            "/api/v1/notification-actions/batch",
            json!({
                "task_ids": p.task_ids,
                "channel_id": p.channel_id,
                "event": p.event.unwrap_or_else(|| "failure".into()),
                "failure_threshold": 1,
                "automatic_only": false,
            }),
        ))
        .await
    }

    #[tool(description = "List the caller's task groups.")]
    async fn list_task_groups(&self) -> Result<String, ErrorData> {
        run(self.client.get("/api/v1/task-groups", &[])).await
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "qdrust-mcp",
    instructions = "Create, run and inspect qdrust HTTP automation tasks."
)]
impl ServerHandler for QdrustMcp {}

#[tokio::main]
async fn main() -> Result<()> {
    let base = std::env::var("QDRUST_URL").unwrap_or_else(|_| "http://localhost:8923".into());
    let token = std::env::var("QDRUST_TOKEN").map_err(|_| {
        anyhow!(
            "QDRUST_TOKEN is required: create a personal access token in qdrust under \
             Settings → API tokens"
        )
    })?;
    let server = QdrustMcp::new(QdrustClient::new(&base, &token)?);
    let service = server
        .serve(stdio())
        .await
        .context("cannot start the MCP server on stdio")?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> QdrustMcp {
        QdrustMcp::new(QdrustClient::new("http://localhost:8923", "qd_test").unwrap())
    }

    /// The tool names are part of the client-facing contract: a rename silently
    /// breaks every saved client config that names a tool, so pin the set.
    #[test]
    fn registers_the_documented_tool_set() {
        let mut names: Vec<String> = server()
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "batch_tasks",
                "bind_notification",
                "cancel_run",
                "create_task",
                "delete_task",
                "get_run_steps",
                "get_task",
                "get_template",
                "import_template",
                "list_notification_channels",
                "list_runs",
                "list_task_groups",
                "list_task_runs",
                "list_tasks",
                "list_templates",
                "run_task",
                "test_template",
                "update_task",
            ]
        );
    }

    /// An updated task must not have an omitted field overwritten: "omit" and
    /// "null" mean different things to the REST API.
    #[test]
    fn update_bodies_omit_fields_the_caller_left_out() {
        let body = object_from([("name", None), ("disabled", Some(json!(true)))]);
        assert_eq!(body, json!({"disabled": true}));
    }

    #[test]
    fn reports_its_own_name_and_version() {
        // The default would be rmcp's own build info, which tells a client
        // nothing about what it is talking to.
        let info = ServerHandler::get_info(&server());
        assert_eq!(info.server_info.name, "qdrust-mcp");
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn truncation_marks_that_it_cut() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdef", 3), "abc…");
    }
}
