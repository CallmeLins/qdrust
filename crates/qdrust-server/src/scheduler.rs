use std::{collections::BTreeMap, str::FromStr, time::Duration};

use chrono::{TimeZone, Utc};
use cron::Schedule;
use qdrust_core::{
    executor::{CancellationToken, ExecutionContext, QdExecutor, StepResult},
    qd_har::{QdHar, QdProgram},
    template::Step,
};
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
                let _ = tick_store.enqueue_run(task.id).await;
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
    let since = Utc
        .timestamp_opt(task.last_run_at.unwrap_or(0), 0)
        .single()
        .unwrap_or(Utc::now() - interval);
    schedule
        .after(&since)
        .next()
        .is_some_and(|next| next <= Utc::now())
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
            let steps = execute_template(template.clone(), &cancellation).await;
            supervisor.abort();
            let steps = steps?;
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
            return Ok::<_, anyhow::Error>(steps.last().map(|step| step.status).unwrap_or(204));
        }
        let method = Method::from_bytes(task.method.as_bytes())?;
        let mut request = client.request(method, &task.url);
        if let Some(headers) = task.headers.as_object() {
            for (name, value) in headers {
                if let Some(value) = value.as_str() {
                    request = request.header(name, value);
                }
            }
        }
        if let Some(body) = &task.body {
            request = request.body(body.clone());
        }
        Ok::<_, anyhow::Error>(request.send().await?.status().as_u16())
    }
    .await;
    match result {
        Ok(status) => {
            info!(task_id = task.id, status, "task completed");
            let _ = store.record_run(task.id, Some(status), None).await;
            let _ = store.finish_run(run.id, Some(status), None).await;
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

async fn execute_template(
    template: Template,
    cancellation: &CancellationToken,
) -> anyhow::Result<Vec<StepResult>> {
    let executor = QdExecutor::new(Duration::from_secs(30))?;
    let mut context = ExecutionContext::new(BTreeMap::new());
    match template.source_format.as_str() {
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
            .map_err(|_| anyhow::anyhow!("execution deadline exceeded"))?
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
            .map_err(|_| anyhow::anyhow!("execution deadline exceeded"))?
        }
        value => anyhow::bail!("unsupported template source format: {value}"),
    }
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
        let results = execute_template(template, &CancellationToken::new())
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
