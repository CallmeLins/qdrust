use std::{collections::BTreeMap, str::FromStr, time::Duration};

use chrono::{TimeZone, Utc};
use cron::Schedule;
use qdrust_core::{
    executor::{ExecutionContext, QdExecutor, StepResult},
    qd_har::{QdHar, QdProgram},
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
    // Periodic maintenance: log retention, expired sessions, expired reset tokens.
    let maint_store = store.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        loop {
            ticker.tick().await;
            let _ = maint_store.purge_expired_sessions().await;
            let _ = maint_store.purge_expired_reset_tokens().await;
            if log_retention_days > 0 {
                let before = Utc::now().timestamp() - (log_retention_days as i64) * 86_400;
                let deleted = maint_store.prune_run_logs(before).await.unwrap_or(0);
                if deleted > 0 {
                    info!(
                        deleted,
                        retention_days = log_retention_days,
                        "pruned run logs"
                    );
                }
            }
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
    run_events: &broadcast::Sender<Value>,
    email: &EmailClient,
) {
    let result = async {
        if let Some(template_id) = task.template_id {
            let template = store
                .get_template(template_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("task template not found"))?;
            let steps = execute_template(template).await?;
            let now = Utc::now().timestamp();
            for (index, step) in steps.iter().enumerate() {
                let step_record = RunStep {
                    id: 0,
                    run_id: run.id,
                    step_index: i64::try_from(index)?,
                    name: format!("step-{}", index + 1),
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
            error!(task_id=task.id, %err, "task failed");
            let message = bounded_error(&err.to_string());
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

async fn execute_template(template: Template) -> anyhow::Result<Vec<StepResult>> {
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
            executor
                .execute_with_deadline(&program, &mut context, Duration::from_secs(300))
                .await
        }
        "native_v1" => {
            let definition = template
                .definition
                .ok_or_else(|| anyhow::anyhow!("native template definition is missing"))?;
            tokio::time::timeout(
                Duration::from_secs(300),
                executor.execute_template(&definition, &mut context),
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
        let results = execute_template(template).await.unwrap();
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
