use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use chrono::{TimeZone, Utc};
use cron::Schedule;
use qdrust_core::{
    executor::{CancellationToken, ExecutionContext, QdExecutor, StepResult},
    plugin::{PLUGIN_API_VERSION, Plugin, PluginManifest as CorePluginManifest, SubprocessPlugin},
    qd_har::{QdHar, QdProgram},
    template::Step,
};
use rand::Rng;
use reqwest::{Client, Method};
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::{
    api::RunEventSender,
    email::{EmailClient, normalize_recipient},
    model::{PluginManifest, RunStep, Task, Template},
    store::Store,
};

pub fn spawn(
    store: Store,
    client: Client,
    interval: Duration,
    run_events: RunEventSender,
    email: EmailClient,
    log_retention_days: u64,
    subscription_sync_interval: Duration,
) {
    let worker_store = store.clone();
    let worker_client = client.clone();
    tokio::spawn(async move {
        let worker = format!("worker-{}", std::process::id());
        loop {
            let _ = worker_store.recover_expired_runs().await;
            match worker_store.claim_run(&worker, 300).await {
                Ok(Some(run)) => {
                    if worker_store
                        .start_leased_run(run.id, &worker)
                        .await
                        .unwrap_or(false)
                        && let Ok(Some(task)) = worker_store.get(run.task_id).await
                    {
                        let _ = run_events.send(Value::from(crate::api::RunEvent {
                            run_id: run.id,
                            kind: "status",
                            status: Some("running".into()),
                            step: None,
                            error: None,
                        }));
                        execute_with_run(
                            worker_store.clone(),
                            worker_client.clone(),
                            task,
                            run,
                            &worker,
                            &run_events,
                            &email,
                        )
                        .await;
                    }
                }
                Ok(None) => tokio::time::sleep(Duration::from_millis(250)).await,
                Err(err) => {
                    error!(%err, "cannot claim run");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    });
    let tick_store = store.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let tasks = match tick_store.list().await {
                Ok(tasks) => tasks,
                Err(err) => {
                    error!(%err, "cannot load tasks");
                    continue;
                }
            };
            for task in tasks.into_iter().filter(|task| due(task, interval)) {
                // QD-style random delay ("当天随机延时区间"): draw the jitter once
                // at enqueue time and let claim_run honor run_after, so the run
                // fires within 0..=max seconds of the scheduled moment instead
                // of hammering the target site at an exact fixed time.
                let max_delay = task.random_delay_max_seconds.unwrap_or(0);
                let result = if max_delay > 0 {
                    let jitter = rand::rng().random_range(0..=max_delay.min(604_800));
                    tick_store.enqueue_delayed_run(task.id, jitter).await
                } else {
                    tick_store.enqueue_run(task.id).await
                };
                let _ = result;
            }
        }
    });
    // Periodic maintenance: log retention (runtime-tunable via site settings),
    // expired sessions, expired reset tokens, expired email verification tokens.
    let maint_store = store.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            let _ = maint_store.purge_expired_sessions().await;
            let _ = maint_store.purge_expired_reset_tokens().await;
            let _ = maint_store.purge_expired_email_tokens().await;
            // Log retention can be tuned at runtime through the site_settings
            // table (admin API) or the config file; the static env value is the fallback.
            let retention = match maint_store
                .get_setting("logs.retention_days")
                .await
                .ok()
                .flatten()
                .and_then(|s| s.value.as_i64())
            {
                Some(days) if days > 0 => days as u64,
                _ => log_retention_days,
            };
            if retention > 0 {
                let before = Utc::now().timestamp() - (retention as i64) * 86_400;
                let deleted = maint_store.prune_run_logs(before).await.unwrap_or(0);
                if deleted > 0 {
                    info!(deleted, retention_days = retention, "pruned run logs");
                }
            }
        }
    });
    // Periodic subscription auto-sync: every enabled subscription is re-synced
    // on the configured interval. sync_subscription records failures in
    // subscription_syncs and logs them, so a bad source never panics the loop.
    // Instances are staggered by a per-subscription offset instead of a
    // distributed lock: syncs are idempotent upserts, so occasional overlap
    // between instances is harmless (comment explains the tradeoff).
    let sync_store = store.clone();
    let sync_client = client.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(subscription_sync_interval);
        loop {
            ticker.tick().await;
            let subscriptions = match sync_store.list_enabled_subscriptions().await {
                Ok(subscriptions) => subscriptions,
                Err(err) => {
                    error!(%err, "cannot load subscriptions for auto-sync");
                    continue;
                }
            };
            let mut synced = 0_usize;
            for subscription in &subscriptions {
                // Stagger each subscription so multiple instances do not hit the
                // same source at the same instant.
                let stagger = Duration::from_secs(subscription.id.unsigned_abs() % 60);
                tokio::time::sleep(stagger).await;
                if crate::subscriptions::sync_subscription(
                    &sync_store,
                    &sync_client,
                    subscription,
                    None,
                )
                .await
                .is_ok()
                {
                    synced += 1;
                }
            }
            info!(
                synced,
                total = subscriptions.len(),
                "automatic subscription sync pass completed"
            );
        }
    });
}

fn due(task: &Task, interval: Duration) -> bool {
    if task.disabled {
        return false;
    }
    let Ok(schedule) = Schedule::from_str(&task.cron) else {
        return false;
    };
    let tz: chrono_tz::Tz = task
        .timezone
        .as_deref()
        .and_then(|tz| tz.parse().ok())
        .unwrap_or(chrono_tz::UTC);
    let now = Utc::now().with_timezone(&tz);
    // A task that has never run must wait for its *next* scheduled moment.
    // Anchoring "since" at now - interval (instead of epoch 0) prevents a new
    // task from firing immediately on the first scheduler tick after creation.
    let since = task
        .last_run_at
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| Utc.timestamp_opt(seconds, 0).single())
        .unwrap_or_else(|| Utc::now() - interval)
        .with_timezone(&tz);
    schedule
        .after(&since)
        .next()
        .is_some_and(|next| next <= now)
}

/// Flatten a task's stored variables object into the BTreeMap the executor
/// expects. Scalar values (string/number/bool) are kept; null and composite
/// values are skipped so a broken variable never fails the whole run.
fn task_variables(task: &Task) -> BTreeMap<String, Value> {
    let mut variables = BTreeMap::new();
    if let Some(Value::Object(map)) = task.variables.as_ref() {
        for (name, value) in map {
            if let Value::String(s) = value {
                variables.insert(name.clone(), Value::String(s.clone()));
            } else if let Value::Number(n) = value {
                variables.insert(name.clone(), Value::String(n.to_string()));
            } else if let Value::Bool(b) = value {
                variables.insert(name.clone(), Value::String(b.to_string()));
            }
        }
    }
    variables
}

#[allow(clippy::too_many_arguments)]
async fn execute_with_run(
    store: Store,
    client: Client,
    task: Task,
    run: crate::model::Run,
    worker: &str,
    run_events: &broadcast::Sender<Value>,
    email: &EmailClient,
) {
    let result = async {
        let variables = task_variables(&task);
        let request_timeout =
            Duration::from_secs(task.timeout_seconds.filter(|v| *v > 0).unwrap_or(30) as u64);
        if let Some(template_id) = task.template_id {
            let template = store
                .get_template(template_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("task template not found"))?;
            // Cancel/lease supervision: a background loop cancels the in-flight
            // execution as soon as the API sets cancel_requested and renews the
            // 300s lease every 60 seconds. It is aborted when execution ends.
            let cancellation = CancellationToken::new();
            let supervisor =
                spawn_run_supervisor(store.clone(), run.id, worker, cancellation.clone());
            // Plugins are resolved per run so an edit or a disable takes effect
            // on the next execution without a restart.
            let plugins = load_plugins(&store, task.id).await;
            let outcome = execute_template(
                template.clone(),
                &cancellation,
                &variables,
                request_timeout,
                &plugins,
            )
            .await;
            supervisor.abort();
            let (steps, final_variables) = outcome?;
            let now = Utc::now().timestamp();
            let methods = template_request_methods(&template);
            for (index, step) in steps.iter().enumerate() {
                let name = match methods.get(index) {
                    Some(method) => truncate_name(&format!("{method} {}", step.url), 200),
                    None => format!("step-{}", index + 1),
                };
                let step_record = RunStep {
                    id: 0,
                    run_id: run.id,
                    step_index: i64::try_from(index)?,
                    name,
                    status: "succeeded".into(),
                    http_status: Some(i64::from(step.status)),
                    body_size: i64::try_from(step.body_size)?,
                    error: None,
                    started_at: now,
                    finished_at: now,
                };
                store.record_run_step(&step_record).await?;
                let _ = run_events.send(Value::from(crate::api::RunEvent {
                    run_id: run.id,
                    kind: "step",
                    status: None,
                    step: Some(serde_json::to_value(&step_record).unwrap_or_default()),
                    error: None,
                }));
            }
            // QD-style log line: the last extraction usually binds __log__ with
            // the human-readable summary ("...签到：获得N积分...").
            let log_message = final_variables
                .get("__log__")
                .and_then(|value| value.as_str())
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
            return Ok::<(u16, Option<String>), anyhow::Error>((
                steps.last().map(|step| step.status).unwrap_or(204),
                log_message,
            ));
        }
        let method = Method::from_bytes(task.method.as_bytes())?;
        let mut request = client
            .request(method, &render_plain(&task.url, &variables)?)
            .timeout(request_timeout);
        if let Some(headers) = task.headers.as_object() {
            for (name, value) in headers {
                if let Some(value) = value.as_str() {
                    let rendered =
                        render_plain(value, &variables).unwrap_or_else(|_| value.to_string());
                    request = request.header(name, rendered);
                }
            }
        }
        if let Some(body) = &task.body {
            request = request.body(render_plain(body, &variables).unwrap_or_else(|_| body.clone()));
        }
        Ok::<(u16, Option<String>), anyhow::Error>((request.send().await?.status().as_u16(), None))
    }
    .await;
    match result {
        Ok((status, log_message)) => {
            info!(task_id = task.id, status, "task completed");
            let _ = store.record_run(task.id, Some(status), None).await;
            let _ = store.finish_run(run.id, Some(status), None).await;
            if let Some(log) = log_message.as_deref() {
                let _ = store.record_run_log(run.id, log).await;
            }
            let _ = run_events.send(Value::from(crate::api::RunEvent {
                run_id: run.id,
                kind: "status",
                status: Some("succeeded".into()),
                step: None,
                error: None,
            }));
            send_notifications(
                &store,
                &client,
                &task,
                run.id,
                "success",
                Some(status),
                None,
                email,
            )
            .await;
        }
        Err(err) => {
            // Include the anyhow cause chain so render errors surface their root
            // cause (e.g. "unknown function a2b_base64") in the run log.
            let message = bounded_error(&format!("{err:#}"));
            if message.contains("execution cancelled") {
                // The run was cancelled through the API while executing. finish_run
                // honours cancel_requested and lands the run in 'cancelled'; do not
                // record a failed step or fire failure notifications.
                let _ = store.finish_run(run.id, None, None).await;
                let _ = run_events.send(Value::from(crate::api::RunEvent {
                    run_id: run.id,
                    kind: "status",
                    status: Some("cancelled".into()),
                    step: None,
                    error: None,
                }));
                return;
            }
            error!(task_id=task.id, %err, "task failed");
            let now = Utc::now().timestamp();
            let _ = store
                .record_run_step(&RunStep {
                    id: 0,
                    run_id: run.id,
                    step_index: 0,
                    name: "execution".into(),
                    status: "failed".into(),
                    http_status: None,
                    body_size: 0,
                    error: Some(message.clone()),
                    started_at: now,
                    finished_at: now,
                })
                .await;
            let _ = store.record_run(task.id, None, Some(&message)).await;
            let _ = store.finish_run(run.id, None, Some(&message)).await;
            let _ = run_events.send(Value::from(crate::api::RunEvent {
                run_id: run.id,
                kind: "status",
                status: Some("failed".into()),
                step: None,
                error: Some(message.clone()),
            }));
            send_notifications(
                &store,
                &client,
                &task,
                run.id,
                "failure",
                None,
                Some(&message),
                email,
            )
            .await;
            // Retry scheduling: enqueue a delayed retry when the task asks for
            // it and the retry chain has not exceeded retry_count (-1 = always).
            let retry_count = task.retry_count.unwrap_or(0);
            if retry_count != 0 {
                let original = run.retry_of.unwrap_or(run.id);
                let done = store.count_retries(original).await.unwrap_or(0);
                let allowed = retry_count == -1 || done < retry_count;
                if allowed {
                    let delay = task.retry_interval_seconds.filter(|v| *v > 0).unwrap_or(60);
                    match store.schedule_retry(task.id, original, delay).await {
                        Ok(Some(retry)) => {
                            info!(
                                task_id = task.id,
                                retry_run = retry.id,
                                delay,
                                "scheduled retry"
                            );
                        }
                        Ok(None) => {}
                        Err(err) => error!(%err, task_id = task.id, "cannot schedule retry"),
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_notifications(
    store: &Store,
    client: &Client,
    task: &Task,
    run_id: i64,
    event: &str,
    http_status: Option<u16>,
    error_message: Option<&str>,
    email: &EmailClient,
) {
    let channels = match store.notification_channels_for_event(task.id, event).await {
        Ok(channels) => channels,
        Err(err) => {
            error!(task_id=task.id, %err, "cannot load notification channels");
            return;
        }
    };
    let payload = serde_json::json!({ "event": event, "task_id": task.id, "task_name": task.name, "run_id": run_id, "http_status": http_status, "error": error_message });
    let status_word = if event == "success" {
        "succeeded"
    } else {
        "failed"
    };
    let title = format!("[qdrust] Task \"{}\" {status_word}", task.name);
    let body = format!(
        "Task: {}\nEvent: {}\nRun: #{}\nHTTP status: {}\nError: {}\n",
        task.name,
        event,
        run_id,
        http_status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".into()),
        error_message.unwrap_or("-"),
    );
    for channel in channels {
        match channel.kind.as_str() {
            "webhook" => {
                let Some(url) = channel.config.get("url").and_then(|value| value.as_str()) else {
                    continue;
                };
                if let Err(err) = client
                    .post(url)
                    .json(&payload)
                    .send()
                    .await
                    .and_then(|response| response.error_for_status())
                {
                    error!(task_id=task.id, channel_id=channel.id, %err, "notification delivery failed");
                }
            }
            "email" => {
                let Some(to) = channel
                    .config
                    .get("to")
                    .and_then(|value| value.as_str())
                    .and_then(normalize_recipient)
                else {
                    continue;
                };
                let from = channel.config.get("from").and_then(|v| v.as_str());
                if let Err(err) = email.send(&to, from, &title, &body) {
                    error!(task_id=task.id, channel_id=channel.id, %err, "email notification failed");
                } else {
                    info!(task_id=task.id, channel_id=channel.id, %to, "email notification sent");
                }
            }
            other if crate::push_channels::is_push_channel(other) => {
                match crate::push_channels::push_to_channel(
                    client,
                    other,
                    &channel.config,
                    &title,
                    &body,
                )
                .await
                {
                    Ok(()) => info!(
                        task_id = task.id,
                        channel_id = channel.id,
                        kind = other,
                        "push notification sent"
                    ),
                    Err(err) => {
                        error!(task_id=task.id, channel_id=channel.id, kind=other, %err, "push notification failed")
                    }
                }
            }
            other => {
                info!(
                    channel_id = channel.id,
                    kind = other,
                    "notification channel sender is not implemented"
                );
            }
        }
    }
}

fn bounded_error(message: &str) -> String {
    const MAX_ERROR_CHARS: usize = 4_096;
    let mut bounded = message.chars().take(MAX_ERROR_CHARS).collect::<String>();
    if message.chars().count() > MAX_ERROR_CHARS {
        bounded.push_str("...");
    }
    bounded
}

/// Poll a running run while it executes: cancel the in-flight execution as soon
/// as `cancel_requested` is set and renew the 300s lease every 60 seconds so
/// long template runs keep their claim. The caller aborts this task when the
/// execution finishes (success, failure or cancellation).
fn spawn_run_supervisor(
    store: Store,
    run_id: i64,
    worker: &str,
    cancellation: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let worker = worker.to_string();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        let mut ticks: u64 = 0;
        loop {
            ticker.tick().await;
            ticks += 1;
            if let Ok(Some(run)) = store.get_run(run_id).await
                && run.cancel_requested
            {
                cancellation.cancel();
                return;
            }
            if ticks.is_multiple_of(60)
                && let Err(err) = store.renew_run(run_id, &worker, 300).await
            {
                error!(%err, run_id, "cannot renew run lease");
            }
        }
    })
}

/// Best-effort per-step HTTP method hints derived from the template source.
/// QD HAR / native steps execute in source order for straight-line programs;
/// loop bodies execute more times than they appear, so the caller falls back to
/// `step-N` when the hint list is exhausted. Display metadata only.
fn template_request_methods(template: &Template) -> Vec<String> {
    let mut methods = Vec::new();
    if template.source_format == "qd_har" {
        if let Some(har) = template.qd_har.as_ref()
            && let Ok(har) = QdHar::parse(har.clone())
        {
            for entry in har.entries() {
                if entry.checked && entry.control().is_none() {
                    methods.push(entry.request.method.clone());
                }
            }
        }
    } else if let Some(definition) = template.definition.as_ref() {
        collect_native_methods(&definition.steps, &mut methods);
    }
    methods
}

fn collect_native_methods(steps: &[Step], methods: &mut Vec<String>) {
    for step in steps {
        match step {
            Step::Request(request) => methods.push(request.method.clone()),
            Step::If {
                then, otherwise, ..
            } => {
                collect_native_methods(then, methods);
                collect_native_methods(otherwise, methods);
            }
            Step::ForEach { steps, .. } => collect_native_methods(steps, methods),
            Step::Extract(_) | Step::Delay { .. } => {}
        }
    }
}

fn truncate_name(name: &str, max: usize) -> String {
    let mut bounded: String = name.chars().take(max).collect();
    if name.chars().count() > max {
        bounded.push_str("...");
    }
    bounded
}

/// Render a plain task field (URL / header / body) with the task's seed
/// variables and the full QD function set. Unrenderable text is passed through
/// verbatim so a stray `{{` never breaks a working task.
fn render_plain(source: &str, variables: &BTreeMap<String, Value>) -> anyhow::Result<String> {
    if !source.contains("{{") {
        return Ok(source.to_string());
    }
    qdrust_core::expression::QdExpressionEngine::default().render(source, variables)
}

/// Load the task owner's enabled plugins as executor-ready plugins.
///
/// A plugin with an unusable command is skipped with a warning instead of
/// failing the run: one broken plugin must not take down every template that
/// never calls it. A task with no owner (pre multi-user data) simply gets none.
async fn load_plugins(store: &Store, task_id: i64) -> Vec<Arc<dyn Plugin>> {
    let owner = match store.task_owner_id(task_id).await {
        Ok(owner) => owner,
        Err(err) => {
            warn!(task_id, %err, "cannot resolve task owner for plugin loading");
            None
        }
    };
    let Some(owner) = owner else {
        return Vec::new();
    };
    let manifests = match store.list_enabled_plugins(owner).await {
        Ok(manifests) => manifests,
        Err(err) => {
            warn!(task_id, %err, "cannot load plugins for task");
            return Vec::new();
        }
    };
    let mut plugins: Vec<Arc<dyn Plugin>> = Vec::with_capacity(manifests.len());
    for manifest in manifests {
        // Same id as the ad-hoc /api/v1/plugins/{id}/invoke route, so the API
        // and a template address a plugin the same way: api://plugin-<id>/<action>.
        let plugin_id = format!("plugin-{}", manifest.id);
        match build_plugin(&manifest, &plugin_id) {
            Ok(plugin) => plugins.push(plugin),
            Err(err) => warn!(task_id, plugin_id = %plugin_id, %err, "skipping plugin"),
        }
    }
    plugins
}

fn build_plugin(manifest: &PluginManifest, plugin_id: &str) -> anyhow::Result<Arc<dyn Plugin>> {
    let capabilities =
        crate::model::plugin_capabilities(&manifest.config).map_err(|err| anyhow::anyhow!(err))?;
    let core_manifest = CorePluginManifest {
        api_version: PLUGIN_API_VERSION,
        id: plugin_id.to_string(),
        name: manifest.name.clone(),
        version: "1".into(),
        capabilities,
    };
    Ok(Arc::new(SubprocessPlugin::from_command(
        core_manifest,
        &manifest.command,
    )?))
}

async fn execute_template(
    template: Template,
    cancellation: &CancellationToken,
    variables: &BTreeMap<String, Value>,
    request_timeout: Duration,
    plugins: &[Arc<dyn Plugin>],
) -> anyhow::Result<(Vec<StepResult>, BTreeMap<String, Value>)> {
    let mut executor = QdExecutor::new(request_timeout)?;
    for plugin in plugins {
        let plugin_id = plugin.manifest().id.clone();
        if let Err(err) = executor.register_plugin(plugin.clone()) {
            // Duplicated ids and invalid manifests are configuration errors;
            // report them and keep going so the run still executes.
            warn!(plugin_id = %plugin_id, %err, "cannot register plugin for this run");
        }
    }
    if !plugins.is_empty() {
        info!(
            plugins = executor.plugin_ids().join(","),
            "registered plugins for run"
        );
    }
    let mut context = ExecutionContext::new(variables.clone());
    let results = match template.source_format.as_str() {
        "qd_har" => {
            let har = QdHar::parse(
                template
                    .qd_har
                    .ok_or_else(|| anyhow::anyhow!("QD HAR source is missing"))?,
            )?;
            let program = QdProgram::compile(&har)?;
            tokio::time::timeout(
                Duration::from_secs(300),
                executor.execute_with_cancellation(&program, &mut context, cancellation),
            )
            .await
            .map_err(|_| anyhow::anyhow!("execution deadline exceeded"))??
        }
        "native_v1" => {
            let definition = template
                .definition
                .ok_or_else(|| anyhow::anyhow!("native template definition is missing"))?;
            tokio::time::timeout(
                Duration::from_secs(300),
                executor.execute_template_with_cancellation(
                    &definition,
                    &mut context,
                    cancellation,
                ),
            )
            .await
            .map_err(|_| anyhow::anyhow!("execution deadline exceeded"))??
        }
        value => anyhow::bail!("unsupported template source format: {value}"),
    };
    Ok((results, context.variables))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn executes_qd_template_through_core() {
        let template = Template {
            id: 1,
            name: "delay".into(),
            description: None,
            schema_version: 1,
            source_format: "qd_har".into(),
            definition: None,
            qd_har: Some(serde_json::json!({
                "log": {
                    "version": "1.2",
                    "entries": [{
                        "checked": true,
                        "request": {"method": "GET", "url": "api://util/delay?seconds=0"}
                    }]
                }
            })),
            created_at: 0,
            updated_at: 0,
            grp: None,
        };
        let (results, _) = execute_template(
            template,
            &CancellationToken::new(),
            &BTreeMap::new(),
            Duration::from_secs(30),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, 200);
    }

    /// In-process plugin used to exercise the whole wiring path (store -> load
    /// -> executor registry -> template assertions -> extracted variables)
    /// without depending on an external executable.
    struct MockPlugin {
        manifest: CorePluginManifest,
    }

    impl Default for MockPlugin {
        fn default() -> Self {
            Self {
                manifest: CorePluginManifest {
                    api_version: PLUGIN_API_VERSION,
                    id: "mock".into(),
                    name: "Mock echo".into(),
                    version: "1.0.0".into(),
                    capabilities: Vec::new(),
                },
            }
        }
    }

    impl Plugin for MockPlugin {
        fn manifest(&self) -> &CorePluginManifest {
            &self.manifest
        }

        fn call<'a>(
            &'a self,
            request: &'a qdrust_core::plugin::PluginRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = anyhow::Result<qdrust_core::plugin::PluginResponse>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                let text = request
                    .query
                    .get("text")
                    .map(String::as_str)
                    .unwrap_or_default();
                Ok(qdrust_core::plugin::PluginResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: format!("echo:{text}").into_bytes(),
                })
            })
        }
    }

    fn mock_template() -> Template {
        Template {
            id: 1,
            name: "plugin echo".into(),
            description: None,
            schema_version: 1,
            source_format: "qd_har".into(),
            definition: None,
            qd_har: Some(serde_json::json!({
                "log": {
                    "version": "1.2",
                    "entries": [{
                        "checked": true,
                        "request": {"method": "GET", "url": "api://mock/echo?text=hello"},
                        "success_asserts": [{"re": "200", "from": "status"}],
                        "extract_variables": [
                            {"name": "echoed", "re": "echo:(.+)", "from": "content"}
                        ]
                    }]
                }
            })),
            created_at: 0,
            updated_at: 0,
            grp: None,
        }
    }

    #[tokio::test]
    async fn executes_qd_har_template_through_a_registered_plugin() {
        let plugins: Vec<Arc<dyn Plugin>> = vec![Arc::new(MockPlugin::default())];
        let (results, variables) = execute_template(
            mock_template(),
            &CancellationToken::new(),
            &BTreeMap::new(),
            Duration::from_secs(30),
            &plugins,
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, 200);
        // The plugin response flowed through extract_variables, so downstream
        // steps (and the __log__ summary) can consume it like any HTTP body.
        assert_eq!(variables.get("echoed"), Some(&json!("hello")));
    }

    #[tokio::test]
    async fn without_plugins_a_plugin_template_fails_with_a_named_plugin() {
        // Regression guard: registering nothing keeps today's behaviour; the
        // only difference is a failure message that names plugin and action.
        let error = execute_template(
            mock_template(),
            &CancellationToken::new(),
            &BTreeMap::new(),
            Duration::from_secs(30),
            &[],
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("plugin unavailable: mock/echo"), "{error}");
        assert!(error.contains("registered: util"), "{error}");
    }

    #[tokio::test]
    async fn loads_only_enabled_plugins_of_the_task_owner() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        store.ready().await.unwrap();
        let owner = store
            .create_user(
                "sched-owner",
                &crate::auth::hash_password("correct horse battery staple").unwrap(),
                "user",
            )
            .await
            .unwrap();
        let task = store
            .create_for_owner(
                owner.id,
                crate::model::CreateTask {
                    name: "plugin-task".into(),
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
                },
            )
            .await
            .unwrap();
        let enabled = store
            .create_plugin(
                owner.id,
                crate::model::CreatePluginManifest {
                    name: "on".into(),
                    command: "plugin-runner --flag".into(),
                    config: serde_json::json!({}),
                    enabled: true,
                },
            )
            .await
            .unwrap();
        store
            .create_plugin(
                owner.id,
                crate::model::CreatePluginManifest {
                    name: "off".into(),
                    command: "plugin-runner".into(),
                    config: serde_json::json!({}),
                    enabled: false,
                },
            )
            .await
            .unwrap();

        let plugins = load_plugins(&store, task.id).await;
        let ids = plugins
            .iter()
            .map(|plugin| plugin.manifest().id.clone())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![format!("plugin-{}", enabled.id)]);

        // Unknown tasks and legacy owner-less tasks load nothing.
        assert!(load_plugins(&store, 999_999).await.is_empty());
    }

    #[test]
    fn build_plugin_reads_declared_capabilities_from_config() {
        let manifest = PluginManifest {
            id: 7,
            name: "echo".into(),
            command: "plugin-runner --flag".into(),
            config: serde_json::json!({"capabilities": ["network"]}),
            enabled: true,
            created_at: 0,
            updated_at: 0,
        };
        let plugin = build_plugin(&manifest, "plugin-7").unwrap();
        assert_eq!(
            plugin.manifest().capabilities,
            vec![qdrust_core::plugin::PluginCapability::Network]
        );

        // A missing key declares nothing (today's behaviour) and an unknown
        // name refuses to build, which load_plugins turns into a warning.
        let plain = PluginManifest {
            config: serde_json::json!({}),
            ..manifest.clone()
        };
        assert!(
            build_plugin(&plain, "plugin-8")
                .unwrap()
                .manifest()
                .capabilities
                .is_empty()
        );
        let typo = PluginManifest {
            config: serde_json::json!({"capabilities": ["netwrok"]}),
            ..manifest
        };
        assert!(build_plugin(&typo, "plugin-9").is_err());
    }

    #[test]
    fn bounds_persisted_error_text() {
        let message = "x".repeat(5_000);
        let bounded = bounded_error(&message);
        assert!(bounded.len() <= 4_099);
        assert!(bounded.ends_with("..."));
    }

    #[test]
    fn normalizes_email_recipients() {
        assert_eq!(normalize_recipient("a@b.com"), Some("a@b.com".into()));
        assert_eq!(
            normalize_recipient("Ops <ops@example.com>"),
            Some("ops@example.com".into())
        );
        assert_eq!(normalize_recipient("not an address"), None);
    }

    fn due_probe_task(cron: &str, last_run_at: Option<i64>, timezone: Option<&str>) -> Task {
        Task {
            id: 1,
            name: "probe".into(),
            cron: cron.into(),
            method: "GET".into(),
            url: "api://util/delay".into(),
            headers: serde_json::json!({}),
            body: None,
            disabled: false,
            created_at: 0,
            updated_at: 0,
            last_run_at,
            last_status: None,
            last_error: None,
            template_id: None,
            grp: None,
            timeout_seconds: None,
            retry_count: None,
            retry_interval_seconds: None,
            priority: None,
            timezone: timezone.map(str::to_string),
            random_delay_max_seconds: None,
            variables: None,
        }
    }

    #[test]
    fn never_run_task_is_not_due_until_scheduled_time() {
        // A task created moments ago (last_run_at = None) must not fire on the
        // next tick just because the schedule had occurrences in the past.
        let task = due_probe_task("0 0 8 * * *", None, Some("Asia/Shanghai"));
        assert!(!due(&task, Duration::from_secs(15)));

        // Same after an explicit epoch-ish sentinel.
        let task = due_probe_task("0 0 8 * * *", Some(0), Some("Asia/Shanghai"));
        assert!(!due(&task, Duration::from_secs(15)));
    }

    #[test]
    fn task_becomes_due_once_scheduled_time_passes() {
        // Simulate: last ran at the previous 08:00 local, now is past the next
        // 08:00 local -> due. We fake this by picking a cron that fires every
        // minute and pretending the last run was 2 intervals ago via a stored
        // timestamp; use UTC directly to keep the assertion deterministic.
        let last_run = (Utc::now() - Duration::from_secs(120)).timestamp();
        let task = due_probe_task("0 * * * * *", Some(last_run), None);
        assert!(due(&task, Duration::from_secs(15)));

        // Last ran a few seconds ago on a daily schedule -> not due yet.
        let last_run = (Utc::now() - Duration::from_secs(30)).timestamp();
        let task = due_probe_task("0 0 8 * * *", Some(last_run), None);
        assert!(!due(&task, Duration::from_secs(15)));
    }

    #[test]
    fn unparsable_cron_and_disabled_tasks_are_never_due() {
        let task = due_probe_task("not a cron", None, None);
        assert!(!due(&task, Duration::from_secs(15)));

        let mut task = due_probe_task("0 * * * * *", None, None);
        task.disabled = true;
        assert!(!due(&task, Duration::from_secs(15)));
    }
}
