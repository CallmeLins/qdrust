use std::{collections::BTreeMap, str::FromStr, time::Duration};

use chrono::{TimeZone, Utc};
use cron::Schedule;
use qdrust_core::{
    executor::{CancellationToken, ExecutionContext, QdExecutor, StepResult},
    qd_har::{QdHar, QdProgram},
    template::Step,
};
use rand::Rng;
use reqwest::{Client, Method};
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{error, info};

use crate::{
    api::RunEventSender,
    email::{EmailClient, normalize_recipient},
    model::{RunStep, Task, Template},
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
    let since = Utc
        .timestamp_opt(task.last_run_at.unwrap_or(0), 0)
        .single()
        .unwrap_or(Utc::now() - interval)
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
            let outcome =
                execute_template(template.clone(), &cancellation, &variables, request_timeout)
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
            let message = bounded_error(&err.to_string());
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
                let subject = format!(
                    "[qdrust] Task \"{}\" {}",
                    task.name,
                    if event == "success" {
                        "succeeded"
                    } else {
                        "failed"
                    }
                );
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
                if let Err(err) = email.send(&to, from, &subject, &body) {
                    error!(task_id=task.id, channel_id=channel.id, %err, "email notification failed");
                } else {
                    info!(task_id=task.id, channel_id=channel.id, %to, "email notification sent");
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

async fn execute_template(
    template: Template,
    cancellation: &CancellationToken,
    variables: &BTreeMap<String, Value>,
    request_timeout: Duration,
) -> anyhow::Result<(Vec<StepResult>, BTreeMap<String, Value>)> {
    let executor = QdExecutor::new(request_timeout)?;
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
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, 200);
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
}
