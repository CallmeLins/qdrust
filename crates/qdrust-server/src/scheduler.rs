use std::{collections::BTreeMap, str::FromStr, sync::Arc, time::Duration};

use chrono::{TimeZone, Utc};
use cron::Schedule;
use qdrust_core::{
    executor::{CancellationToken, ExecutionContext, ExecutorOptions, QdExecutor, StepResult},
    plugin::{PLUGIN_API_VERSION, Plugin, PluginManifest as CorePluginManifest, SubprocessPlugin},
    qd_har::{QdHar, QdProgram},
    template::Step,
};
use qdrust_plugin_browser::{BrowserSessionManager, BrowserSessionPlugin};
use rand::Rng;
use reqwest::Method;
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::{
    api::RunEventSender,
    delivery::{Message, TemplateVars, format_notification_time},
    email::EmailClient,
    model::{PluginManifest, RunStep, Task, Template},
    outbound::OutboundHttp,
    store::Store,
};

#[allow(clippy::too_many_arguments)]
pub fn spawn(
    store: Store,
    outbound: OutboundHttp,
    interval: Duration,
    run_events: RunEventSender,
    email: EmailClient,
    log_retention_days: u64,
    settings: Arc<std::sync::RwLock<crate::api::RuntimeSettings>>,
    browser: Option<Arc<BrowserSessionManager>>,
    default_tz: chrono_tz::Tz,
) {
    let worker_store = store.clone();
    let worker_outbound = outbound.clone();
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
                        // Read the admin's network policy per run rather than
                        // snapshotting it at boot, so flipping the switch in the
                        // UI takes effect on the next run, not the next restart.
                        let policy = run_policy(task.timeout_seconds, &settings);
                        execute_with_run(
                            worker_store.clone(),
                            worker_outbound.clone(),
                            task,
                            run,
                            &worker,
                            &run_events,
                            &email,
                            policy,
                            browser.clone(),
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
            for task in tasks
                .into_iter()
                .filter(|task| due(task, interval, default_tz))
            {
                // QD-style random delay ("当天随机延时区间"): draw the jitter once
                // at enqueue time and let claim_run honor run_after, so the run
                // fires within 0..=max seconds of the scheduled moment instead
                // of hammering the target site at an exact fixed time.
                let max_delay = task.random_delay_max_seconds.unwrap_or(0);
                let result = if max_delay > 0 {
                    let jitter = rand::rng().random_range(0..=max_delay.min(604_800));
                    tick_store.enqueue_delayed_run(task.id, jitter).await
                } else {
                    tick_store
                        .enqueue_run_with_trigger(task.id, "scheduled")
                        .await
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
            // OIDC single-use login states (OIDC_STATE_TTL_SECS) that were never
            // consumed (user aborted mid-flow) are garbage-collected here too.
            let _ = maint_store.purge_expired_oidc_login_states().await;
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
}

/// Whether `task` should fire on this scheduler tick, evaluated in the task's
/// own IANA timezone when set, otherwise the server-wide `default_tz` (from
/// `QDRUST_DEFAULT_TIMEZONE`, itself defaulting to `Asia/Shanghai`).
fn due(task: &Task, interval: Duration, default_tz: chrono_tz::Tz) -> bool {
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
        .unwrap_or(default_tz);
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

/// What a single run is allowed to do, as opposed to what its template asks
/// for.
///
/// Grouped into one value so a new policy knob cannot be threaded into one call
/// site and forgotten at another: the executor is built from exactly one of
/// these per run, in one place.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunPolicy {
    /// Per-task request timeout (`task.timeout_seconds`, default 30s).
    request_timeout: Duration,
    /// ADR-0008 private-network opt-in.
    allow_private_network: bool,
    /// ADR-0008 invalid-certificate opt-in. A sibling of the flag above, not a
    /// consequence of it: they are granted separately.
    allow_invalid_certificates: bool,
}

/// The policy a run executes under, assembled from the task and the shared
/// runtime settings.
///
/// Read per run rather than once at boot: that is what makes flipping the admin
/// switch apply to the next run instead of the next restart. A snapshot taken
/// at startup would compile, look correct, and quietly require a restart — the
/// reason this is a named function with a test rather than an inline read.
pub(crate) fn run_policy(
    timeout_seconds: Option<i64>,
    settings: &std::sync::RwLock<crate::api::RuntimeSettings>,
) -> RunPolicy {
    // One read for both switches rather than one per field: this is a blocking
    // lock taken from async code, so it is held for the shortest span that
    // still yields a consistent pair.
    let snapshot = settings.read().unwrap();
    RunPolicy {
        request_timeout: Duration::from_secs(
            timeout_seconds.filter(|v| *v > 0).unwrap_or(30) as u64
        ),
        allow_private_network: snapshot.allow_private_network,
        allow_invalid_certificates: snapshot.allow_invalid_certificates,
    }
}

/// The second of the two ways a run executes: a request the user composed out
/// of a method, a URL, headers and a body, with no template involved.
///
/// Split out from [`execute_with_run`] so it can be tested as what it is — the
/// most ordinary way to ask this server to fetch something, and the one that
/// used to have no guard at all. Every part of the request is user-supplied, so
/// it goes through the same guarded client a template run does and takes the
/// same per-run timeout.
///
/// Returns the status code, which is all this path has ever reported.
async fn execute_plain_task(
    outbound: &OutboundHttp,
    task: &Task,
    variables: &BTreeMap<String, Value>,
    policy: RunPolicy,
) -> anyhow::Result<u16> {
    let method = Method::from_bytes(task.method.as_bytes())?;
    let target = render_plain(&task.url, variables)?;
    let mut request = outbound
        .request(method, &target)
        .await?
        .timeout(policy.request_timeout);
    if let Some(headers) = task.headers.as_object() {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                let rendered = render_plain(value, variables).unwrap_or_else(|_| value.to_string());
                request = request.header(name, rendered);
            }
        }
    }
    if let Some(body) = &task.body {
        request = request.body(render_plain(body, variables).unwrap_or_else(|_| body.clone()));
    }
    Ok(request.send().await?.status().as_u16())
}

#[allow(clippy::too_many_arguments)]
async fn execute_with_run(
    store: Store,
    outbound: OutboundHttp,
    task: Task,
    run: crate::model::Run,
    worker: &str,
    run_events: &broadcast::Sender<Value>,
    email: &EmailClient,
    policy: RunPolicy,
    browser: Option<Arc<BrowserSessionManager>>,
) {
    let result = async {
        let variables = task_variables(&task);
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
            let plugins = load_plugins(&store, task.id, browser).await;
            let outcome = execute_template(
                template.clone(),
                &cancellation,
                &variables,
                policy,
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
        Ok::<(u16, Option<String>), anyhow::Error>((
            execute_plain_task(&outbound, &task, &variables, policy).await?,
            None,
        ))
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
                &outbound,
                &task,
                run.id,
                "success",
                Some(status),
                None,
                log_message.as_deref(),
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
            // The anyhow chain, not just the top context: a bare "cannot render
            // QD template value" told a reporter nothing; the root cause (e.g.
            // "unknown method: sequence has no method named append (in <string>:6)")
            // is what they paste back to the issue.
            error!(task_id = task.id, err = format!("{err:#}"), "task failed");
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
                &outbound,
                &task,
                run.id,
                "failure",
                None,
                Some(&message),
                None,
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
    outbound: &OutboundHttp,
    task: &Task,
    run_id: i64,
    event: &str,
    http_status: Option<u16>,
    error_message: Option<&str>,
    log_message: Option<&str>,
    email: &EmailClient,
) {
    let deliveries = match store.notification_channels_for_event(task.id, event).await {
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
    let run = store.get_run(run_id).await.ok().flatten();
    let automatic = run.as_ref().is_some_and(|run| run.trigger != "manual");
    let failure_count = if event == "failure" {
        store.count_recent_failures(task.id).await.unwrap_or(1)
    } else {
        0
    };
    for delivery in deliveries {
        let action = delivery.action;
        if action.automatic_only && !automatic {
            continue;
        }
        if event == "failure" && failure_count < action.failure_threshold {
            continue;
        }
        let log = log_message
            .or_else(|| run.as_ref().and_then(|run| run.log.as_deref()))
            .unwrap_or("");
        let vars = TemplateVars {
            event,
            task_id: task.id,
            task_name: &task.name,
            run_id,
            status: http_status.map(|s| s.to_string()).unwrap_or_default(),
            error: error_message.unwrap_or(""),
            log,
            // Prefer the finish time, fall back to the enqueue time, and only
            // then to "now" (a run row that vanished from the store).
            time: format_notification_time(
                run.as_ref()
                    .map(|run| run.finished_at.unwrap_or(run.created_at))
                    .unwrap_or_else(|| Utc::now().timestamp()),
                task.timezone.as_deref(),
            ),
        };
        let channel = delivery.channel;
        let title = action
            .title_template
            .as_deref()
            .map(|value| vars.render(value))
            .unwrap_or_else(|| title.clone());
        let body = action
            .body_template
            .as_deref()
            .map(|value| vars.render(value))
            .unwrap_or_else(|| body.clone());
        let message = Message {
            title: &title,
            body: &body,
            payload: &payload,
        };
        match crate::delivery::deliver(
            outbound,
            &channel.kind,
            &channel.config,
            &message,
            &|value| vars.render(value),
            Some(email),
        )
        .await
        {
            Ok(()) => info!(
                task_id = task.id,
                channel_id = channel.id,
                kind = %channel.kind,
                "notification delivered"
            ),
            Err(err) => error!(
                task_id = task.id,
                channel_id = channel.id,
                kind = %channel.kind,
                %err,
                "notification delivery failed"
            ),
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
async fn load_plugins(
    store: &Store,
    task_id: i64,
    browser: Option<Arc<BrowserSessionManager>>,
) -> Vec<Arc<dyn Plugin>> {
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
    load_plugins_for_owner(store, owner, browser).await
}

/// The same plugin set a run of this owner would get, without needing a task:
/// used by the template test endpoint, which must execute a template exactly as
/// a real run would.
///
/// `browser` is `None` there because the process-level browser manager is held
/// by the scheduler, not the API; a template whose step needs `api://browser/*`
/// reports the plugin as unavailable in a test run, which is honest rather than
/// silently skipping the step.
pub(crate) async fn load_plugins_for_owner(
    store: &Store,
    owner: i64,
    browser: Option<Arc<BrowserSessionManager>>,
) -> Vec<Arc<dyn Plugin>> {
    let manifests = match store.list_enabled_plugins(owner).await {
        Ok(manifests) => manifests,
        Err(err) => {
            warn!(owner, %err, "cannot load plugins for owner");
            return Vec::new();
        }
    };
    let mut plugins: Vec<Arc<dyn Plugin>> = Vec::with_capacity(manifests.len() + 1);
    for manifest in manifests {
        // Same id as the ad-hoc /api/v1/plugins/{id}/invoke route, so the API
        // and a template address a plugin the same way: api://plugin-<id>/<action>.
        let plugin_id = format!("plugin-{}", manifest.id);
        match build_plugin(&manifest, &plugin_id) {
            Ok(plugin) => plugins.push(plugin),
            Err(err) => warn!(owner, plugin_id = %plugin_id, %err, "skipping plugin"),
        }
    }
    // The optional browser plugin is wired in when the process owns a headless
    // browser session manager (constructed in main.rs when QDRUST_BROWSER_URL
    // is set). The manager runs chromiumoxide in-process and keeps sessions
    // alive across calls, so a template addresses it directly as
    // `api://browser/<action>` without a database plugin entry.
    if let Some(manager) = browser {
        plugins.push(Arc::new(BrowserSessionPlugin::new(manager)) as Arc<dyn Plugin>);
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

pub(crate) async fn execute_template(
    template: Template,
    cancellation: &CancellationToken,
    variables: &BTreeMap<String, Value>,
    policy: RunPolicy,
    plugins: &[Arc<dyn Plugin>],
) -> anyhow::Result<(Vec<StepResult>, BTreeMap<String, Value>)> {
    // The one place a run's executor is built, so a policy knob cannot be
    // honoured on one path (a QD template) and quietly ignored on the other
    // (a plain request). Everything except the timeout and ADR-0008's two
    // relaxations keeps the hardened defaults.
    let mut executor = QdExecutor::with_options(ExecutorOptions {
        timeout: policy.request_timeout,
        allow_private_network: policy.allow_private_network,
        allow_invalid_certificates: policy.allow_invalid_certificates,
        ..ExecutorOptions::default()
    })?;
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
    use crate::email::normalize_recipient;
    use crate::test_support::{LOOPBACK_MARKER, LOOPBACK_STATUS, serve_loopback};
    use serde_json::json;
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A run policy for tests: 30s timeout, private network off unless asked,
    /// certificates always verified. The certificate relaxation gets its own
    /// constructor so no existing call site silently gains it.
    fn policy(allow_private_network: bool) -> RunPolicy {
        RunPolicy {
            request_timeout: Duration::from_secs(30),
            allow_private_network,
            allow_invalid_certificates: false,
        }
    }

    /// An `OutboundHttp` whose settings say what the test needs them to say.
    ///
    /// A fresh settings handle per call rather than a shared one that gets
    /// flipped, so a test states its posture in one line and cannot leave it
    /// behind for another test to inherit.
    fn guarded(allow_private_network: bool, allow_invalid_certificates: bool) -> OutboundHttp {
        let settings = crate::api::runtime_settings();
        {
            let mut runtime = settings.write().unwrap();
            runtime.allow_private_network = allow_private_network;
            runtime.allow_invalid_certificates = allow_invalid_certificates;
        }
        OutboundHttp::new(settings, Duration::from_secs(30))
    }

    /// A task with no template: the plain-URL execution path, complete with a
    /// method, a body and no template to fall back on.
    fn plain_task(url: String) -> Task {
        Task {
            method: "POST".into(),
            url,
            body: Some("{\"probe\":true}".into()),
            ..due_probe_task("0 0 8 * * *", None, None)
        }
    }

    /// The same policy with ADR-0008's other relaxation on. Takes the
    /// private-network flag too, because reaching a loopback test server needs
    /// both — which is itself the point of the HTTPS test below.
    fn policy_accepting_invalid_certs(allow_private_network: bool) -> RunPolicy {
        RunPolicy {
            allow_invalid_certificates: true,
            ..policy(allow_private_network)
        }
    }

    #[test]
    fn the_run_policy_reads_the_live_setting_instead_of_a_boot_snapshot() {
        // Flipping the admin switch has to reach the next run. A value captured
        // at startup would leave a restart in between, which reads as the switch
        // not working. Both switches, since they are read in the same function.
        let settings = crate::api::runtime_settings();
        assert!(!run_policy(None, &settings).allow_private_network);
        assert!(!run_policy(None, &settings).allow_invalid_certificates);
        settings.write().unwrap().allow_private_network = true;
        settings.write().unwrap().allow_invalid_certificates = true;
        assert!(run_policy(None, &settings).allow_private_network);
        assert!(run_policy(None, &settings).allow_invalid_certificates);
    }

    #[test]
    fn the_run_policy_takes_the_task_timeout_and_defaults_the_rest() {
        let settings = crate::api::runtime_settings();
        assert_eq!(
            run_policy(Some(45), &settings).request_timeout,
            Duration::from_secs(45)
        );
        // Unset and non-positive both fall back to 30s.
        for value in [None, Some(0), Some(-5)] {
            assert_eq!(
                run_policy(value, &settings).request_timeout,
                Duration::from_secs(30)
            );
        }
    }

    #[test]
    fn one_relaxation_never_drags_the_other_along() {
        // The pair is read from one snapshot, so a copy-paste slip could wire
        // both fields to whichever flag was written first. With both settings
        // off and only the private-network flag set, the certificate field has
        // to stay off.
        let settings = crate::api::runtime_settings();
        settings.write().unwrap().allow_private_network = true;
        let policy = run_policy(None, &settings);
        assert!(policy.allow_private_network);
        assert!(!policy.allow_invalid_certificates);
    }

    #[test]
    fn the_worker_assembles_that_policy_once_per_claim() {
        // Structural, and deliberately so: the tests above cover `run_policy`
        // and the loopback test covers the executor, but neither covers the
        // worker *calling* it. A snapshot hoisted out of the loop would pass
        // everything else and silently reintroduce the restart requirement.
        //
        // The needle is assembled from parts because this file is what
        // `include_str!` reads: written as one literal it would also match the
        // assertion's own source, and the test would pass whether or not the
        // worker still calls it. Keep it split.
        let needle = format!(
            "{}{}",
            "let policy = run_policy(", "task.timeout_seconds, &settings);"
        );
        assert!(include_str!("scheduler.rs").contains(&needle));
    }

    /// The worker must not build a client of its own for either branch it owns.
    ///
    /// What this covers is a hop, not a function: the worker handing its
    /// `OutboundHttp` to the plain-URL branch and to the notification branches.
    /// `execute_plain_task` and `deliver` are each tested with a client passed
    /// in, so a worker that reached for `OutboundHttp::standalone()` instead
    /// would leave every other test green — and that is the shape of the bug
    /// this whole change is about: one client, four paths, one guard.
    ///
    /// Structural, and phrased as "not built here" rather than "passes this
    /// argument", so it depends on neither an argument order nor a line break.
    /// The production half is cut at the first `#[cfg(test)]`, which is where
    /// this module begins; the assertion's own literal therefore cannot match
    /// itself.
    #[test]
    fn the_worker_builds_no_client_of_its_own() {
        let source = include_str!("scheduler.rs").replace("\r\n", "\n");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("the file is never empty");
        assert!(
            !production.contains("OutboundHttp::standalone()"),
            "the worker built its own client; every branch takes the one it was handed"
        );
    }

    #[test]
    fn the_executor_is_built_with_both_relaxations_from_the_policy() {
        // Structural for the same reason as the test above: `qdrust-core` cannot
        // cover this, because there the options are handed to it directly. What
        // is unverified until this assertion exists is the step where the
        // policy's two flags become `ExecutorOptions` fields — the exact place
        // the original bug lived (`QdExecutor::new` never set either).
        let source = include_str!("scheduler.rs");
        for needle in [
            format!(
                "{}{}",
                "allow_private_network: policy.", "allow_private_network,"
            ),
            format!(
                "{}{}",
                "allow_invalid_certificates: policy.", "allow_invalid_certificates,"
            ),
        ] {
            assert!(
                source.contains(&needle),
                "the executor no longer takes {} from the policy",
                needle.trim_end_matches(',')
            );
        }
    }

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
            variables: Vec::new(),
            variable_defaults: Default::default(),
            created_at: 0,
            updated_at: 0,
            task_count: 0,
            grp: None,
        };
        let (results, _) = execute_template(
            template,
            &CancellationToken::new(),
            &BTreeMap::new(),
            policy(false),
            &[],
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, 200);
    }

    /// A one-step template that GETs `url` and extracts the marker from the
    /// body. The URL is passed in rather than built from an address so the same
    /// shape serves both the plain-HTTP and the HTTPS tests.
    fn loopback_template(url: String) -> Template {
        Template {
            id: 1,
            name: "loopback".into(),
            description: None,
            schema_version: 1,
            source_format: "qd_har".into(),
            definition: None,
            qd_har: Some(serde_json::json!({
                "log": {
                    "version": "1.2",
                    "entries": [{
                        "checked": true,
                        "request": {"method": "GET", "url": url},
                        "extract_variables": [
                            {"name": "marker", "re": "(from-the-loopback-test-server)", "from": "content"}
                        ]
                    }]
                }
            })),
            variables: Vec::new(),
            variable_defaults: Default::default(),
            created_at: 0,
            updated_at: 0,
            task_count: 0,
            grp: None,
        }
    }

    /// The regression this switch exists for: a template that needs a service on
    /// the host's own network (a local flaresolverr, an intranet sign-in page).
    /// With the flag off the run is refused; with it on the same template
    /// completes.
    ///
    /// What this covers is the *wiring* — that the admin setting reaches
    /// `ExecutorOptions.allow_private_network`. `qdrust-core`'s own tests cannot
    /// cover it, because there the flag is set straight on the options.
    #[tokio::test]
    async fn a_private_target_is_refused_until_the_admin_allows_it() {
        let address = serve_loopback().await;

        let blocked = execute_template(
            loopback_template(format!("http://{address}/")),
            &CancellationToken::new(),
            &BTreeMap::new(),
            policy(false),
            &[],
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            blocked.contains("private or special-use network target is blocked"),
            "with the flag off a loopback target must be refused: {blocked}"
        );

        let (results, variables) = execute_template(
            loopback_template(format!("http://{address}/")),
            &CancellationToken::new(),
            &BTreeMap::new(),
            policy(true),
            &[],
        )
        .await
        .expect("with the flag on the same template must go through");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, LOOPBACK_STATUS);
        // The body has to be the one this test's server wrote.
        assert_eq!(variables.get("marker"), Some(&json!(LOOPBACK_MARKER)));
    }

    /// The path this whole guard started from, and the one it was missing for
    /// longest: a task with no template is a single request the user composed,
    /// and it reached the network with no policy at all — no address check, no
    /// pinning, no switch. Reaching a private address from it is the same
    /// primitive as reaching one from a template.
    ///
    /// The status is the only thing this path reports, so the test server also
    /// counts requests: the refusal has to leave the count at zero, which is
    /// stronger than any status comparison — nothing was sent at all.
    #[tokio::test]
    async fn a_plain_task_url_is_guarded_like_a_template_run() {
        let (address, served) = crate::test_support::serve_counting_loopback().await;
        let task = plain_task(format!("http://{address}/"));

        let blocked = execute_plain_task(
            &guarded(false, false),
            &task,
            &BTreeMap::new(),
            policy(false),
        )
        .await
        .unwrap_err();
        // The whole chain: the guard's reason is the cause of the message this
        // module wraps it in, so `{}` alone would show only the wrapper.
        let blocked = format!("{blocked:#}");
        assert!(
            blocked.contains("private or special-use network target is blocked"),
            "a task URL pointing at loopback must be refused while the switch is off: {blocked}"
        );
        assert_eq!(
            served.load(Ordering::SeqCst),
            0,
            "a refused task must not reach the network at all"
        );

        let status =
            execute_plain_task(&guarded(true, false), &task, &BTreeMap::new(), policy(true))
                .await
                .expect("with the switch on the same task must go through");
        assert_eq!(
            status, LOOPBACK_STATUS,
            "the status has to be the one this test's server sent"
        );
        assert_eq!(
            served.load(Ordering::SeqCst),
            1,
            "the allowed attempt must have reached this socket"
        );
    }

    /// Self-signed certificate and its matching key, DER, base64.
    ///
    /// Generated once (Git Bash; `MSYS_NO_PATHCONV=1` keeps openssl from
    /// mangling the `/CN=` argument) and pasted here, so the test needs no
    /// fixture file, no certificate authority and no generation step at run
    /// time:
    ///
    /// ```text
    /// openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
    ///   -nodes -keyout key.pem -out cert.pem -days 36500 -subj "/CN=localhost" \
    ///   -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
    /// openssl x509 -in cert.pem -outform DER | base64 -w0     # SELF_SIGNED_CERT_DER
    /// openssl pkcs8 -topk8 -nocrypt -in key.pem -outform DER \
    ///   | base64 -w0                                          # SELF_SIGNED_KEY_DER
    /// ```
    ///
    /// EC rather than RSA only to keep the two constants short. The key is a
    /// throwaway that exists solely to be rejected by every verifier in the
    /// world — not a secret, and used nowhere else.
    const SELF_SIGNED_CERT_DER: &str = "MIIBmjCCAUGgAwIBAgIUOk7XR2G9pRfJWNW6JvvdHND1A4EwCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJbG9jYWxob3N0MCAXDTI2MDkyMTEwNTgzN1oYDzIxMjYwODI4MTA1ODM3WjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASldseGX52lLRbHOCxxNbLCjoqd3Vnv92juDGjVX6q7KtFQq3DRYff09TNRaxYtLLNBjiXxE5fyiPSijepVPhdBo28wbTAdBgNVHQ4EFgQUKAS3Xy5p7OxAaJx3BLtPevh2CegwHwYDVR0jBBgwFoAUKAS3Xy5p7OxAaJx3BLtPevh2CegwDwYDVR0TAQH/BAUwAwEB/zAaBgNVHREEEzARgglsb2NhbGhvc3SHBH8AAAEwCgYIKoZIzj0EAwIDRwAwRAIgHja5JnnGPw+caWkvhrKGzcn6+88SSw/yaf0BvNFRZU8CIFHKcAL4Ex3Y+5x6RzgidTydMJ5b/gBOZDIGWyFUWEqg";
    const SELF_SIGNED_KEY_DER: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgrSnuGF4NX83+l5YpganV3mqCNlogXcyHm4ZY0knSRrqhRANCAASldseGX52lLRbHOCxxNbLCjoqd3Vnv92juDGjVX6q7KtFQq3DRYff09TNRaxYtLLNBjiXxE5fyiPSijepVPhdB";

    /// HTTPS server presenting that self-signed certificate, answering with the
    /// loopback marker. Same marker reasoning as the plain server above: a run
    /// that returns 200 must have talked to *this* socket.
    ///
    /// Handshake failures are expected (that is what the certificate switch is
    /// about) and are swallowed, so the listener keeps serving.
    async fn serve_self_signed_marker() -> std::net::SocketAddr {
        use base64::Engine as _;
        use tokio_rustls::TlsAcceptor;
        use tokio_rustls::rustls::{ServerConfig, crypto::ring, pki_types};

        let engine = base64::engine::general_purpose::STANDARD;
        let certificate = pki_types::CertificateDer::from(
            engine
                .decode(SELF_SIGNED_CERT_DER)
                .expect("certificate is base64"),
        );
        let key = pki_types::PrivateKeyDer::Pkcs8(pki_types::PrivatePkcs8KeyDer::from(
            engine.decode(SELF_SIGNED_KEY_DER).expect("key is base64"),
        ));
        // The provider is passed explicitly rather than installed process-wide:
        // tests share a process, and a global install would be a side effect
        // other tests could trip over.
        let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .expect("the pasted certificate and key match");
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(socket).await else {
                        // A client that verifies certificates gives up here.
                        return;
                    };
                    let mut scratch = [0_u8; 2048];
                    let _ = stream.read(&mut scratch).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        LOOPBACK_MARKER.len(),
                        LOOPBACK_MARKER
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        address
    }

    /// ADR-0008's second switch, and the proof that it is a switch of its own
    /// rather than a passenger on the first.
    ///
    /// The target is a self-signed HTTPS endpoint on loopback, so one request
    /// exercises both relaxations at once: reaching the address needs the
    /// network switch, accepting the certificate needs this one. With only the
    /// network switch on the run still fails — visibly, and for a reason that
    /// is emphatically *not* the SSRF guard, which is what makes this a test of
    /// the second switch rather than a repeat of the first.
    ///
    /// What it covers is the wiring: that the admin setting reaches
    /// `ExecutorOptions.allow_invalid_certificates`. `qdrust-core` cannot cover
    /// it — there the flag is set straight on the options, and its own tests
    /// never exercise `danger_accept_invalid_certs` at all.
    #[tokio::test]
    async fn a_self_signed_target_needs_the_certificate_switch_too() {
        let address = serve_self_signed_marker().await;
        let url = format!("https://{address}/");

        // Network switch alone: the address is allowed through and TLS then
        // refuses the certificate. The error must not be the SSRF guard's.
        let rejected = execute_template(
            loopback_template(url.clone()),
            &CancellationToken::new(),
            &BTreeMap::new(),
            policy(true),
            &[],
        )
        .await
        .expect_err("a self-signed certificate must not be accepted by default");
        // `{:#}` prints anyhow's whole chain, which is where reqwest puts the
        // rustls cause; `{}` would only show "error sending request for url".
        let chain = format!("{rejected:#}");
        assert!(
            !chain.contains("private or special-use network target is blocked"),
            "the network switch alone must let the address through to TLS: {chain}"
        );
        assert!(
            chain.contains("certificate"),
            "the failure must be the certificate, not something else: {chain}"
        );

        // Both switches on: same template, same server, completes — and the
        // body is the one this test's server sent.
        let (results, variables) = execute_template(
            loopback_template(url),
            &CancellationToken::new(),
            &BTreeMap::new(),
            policy_accepting_invalid_certs(true),
            &[],
        )
        .await
        .expect("with the certificate switch on the same template must go through");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, 200);
        assert_eq!(variables.get("marker"), Some(&json!(LOOPBACK_MARKER)));
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
            variables: Vec::new(),
            variable_defaults: Default::default(),
            created_at: 0,
            updated_at: 0,
            task_count: 0,
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
            policy(false),
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
            policy(false),
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

        let plugins = load_plugins(&store, task.id, None).await;
        let ids = plugins
            .iter()
            .map(|plugin| plugin.manifest().id.clone())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![format!("plugin-{}", enabled.id)]);

        // Unknown tasks and legacy owner-less tasks load nothing.
        assert!(load_plugins(&store, 999_999, None).await.is_empty());
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
        assert!(!due(
            &task,
            Duration::from_secs(15),
            chrono_tz::Asia::Shanghai
        ));

        // Same after an explicit epoch-ish sentinel.
        let task = due_probe_task("0 0 8 * * *", Some(0), Some("Asia/Shanghai"));
        assert!(!due(
            &task,
            Duration::from_secs(15),
            chrono_tz::Asia::Shanghai
        ));
    }

    #[test]
    fn task_becomes_due_once_scheduled_time_passes() {
        // Simulate: last ran at the previous 08:00 local, now is past the next
        // 08:00 local -> due. We fake this by picking a cron that fires every
        // minute and pretending the last run was 2 intervals ago via a stored
        // timestamp; use UTC directly to keep the assertion deterministic.
        let last_run = (Utc::now() - Duration::from_secs(120)).timestamp();
        let task = due_probe_task("0 * * * * *", Some(last_run), None);
        assert!(due(&task, Duration::from_secs(15), chrono_tz::UTC));

        // Last ran a few seconds ago on a daily schedule -> not due yet.
        let last_run = (Utc::now() - Duration::from_secs(30)).timestamp();
        let task = due_probe_task("0 0 8 * * *", Some(last_run), None);
        assert!(!due(&task, Duration::from_secs(15), chrono_tz::UTC));
    }

    #[test]
    fn unparsable_cron_and_disabled_tasks_are_never_due() {
        let task = due_probe_task("not a cron", None, None);
        assert!(!due(&task, Duration::from_secs(15), chrono_tz::UTC));

        let mut task = due_probe_task("0 * * * * *", None, None);
        task.disabled = true;
        assert!(!due(&task, Duration::from_secs(15), chrono_tz::UTC));
    }

    #[test]
    fn browser_plugin_manifest_declares_network_capability() {
        // The in-process BrowserSessionPlugin is constructed from a configured
        // manager. An unconfigured manager (blank endpoint) still builds a
        // valid plugin; it simply fails loudly on connect. Only the manifest
        // shape matters here.
        let manager = Arc::new(BrowserSessionManager::new(Some(
            "ws://localhost:3000".to_string(),
        )));
        let plugin = BrowserSessionPlugin::new(manager);
        assert_eq!(plugin.manifest().id, "browser");
        assert_eq!(
            plugin.manifest().capabilities,
            vec![qdrust_core::plugin::PluginCapability::Network]
        );
    }

    #[test]
    fn browser_session_manager_is_configured_only_with_endpoint() {
        assert!(!BrowserSessionManager::new(None).is_configured());
        assert!(BrowserSessionManager::new(Some("  ".to_string())).is_configured());
        assert!(
            BrowserSessionManager::new(Some("http://localhost:9222".to_string())).is_configured()
        );
    }

    fn template_vars<'a>(
        event: &'a str,
        error: &'a str,
        log: &'a str,
        status: Option<u16>,
    ) -> TemplateVars<'a> {
        TemplateVars {
            event,
            task_id: 7,
            task_name: "daily check",
            run_id: 42,
            status: status.map(|s| s.to_string()).unwrap_or_default(),
            error,
            log,
            time: "2026-09-11 20:00:00".into(),
        }
    }

    #[test]
    fn notification_template_substitutes_every_variable() {
        let vars = template_vars("failure", "boom", "line one", Some(500));
        assert_eq!(
            vars.render("{event}|{task_id}|{task}|{run_id}|{status}|{error}|{log}|{t}"),
            "failure|7|daily check|42|500|boom|line one|2026-09-11 20:00:00"
        );
    }

    #[test]
    fn notification_template_keeps_unknown_placeholders_and_blanks_missing_status() {
        let vars = template_vars("success", "", "", None);
        // `{status}` collapses to empty when the run has no HTTP status, and an
        // unknown placeholder is preserved verbatim rather than dropped.
        assert_eq!(vars.render("s={status} {nope}"), "s= {nope}");
    }

    #[test]
    fn notification_time_uses_the_task_timezone() {
        // Epoch 0 renders in the task's zone, falling back to UTC for a task
        // without a timezone or with an unparsable one.
        assert_eq!(
            format_notification_time(0, Some("Asia/Shanghai")),
            "1970-01-01 08:00:00"
        );
        assert_eq!(format_notification_time(0, None), "1970-01-01 00:00:00");
        assert_eq!(
            format_notification_time(0, Some("Mars/Olympus")),
            "1970-01-01 00:00:00"
        );
    }
}
