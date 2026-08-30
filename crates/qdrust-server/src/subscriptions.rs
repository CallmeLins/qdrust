use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use qdrust_core::qd_har::QdHar;
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::{model::TemplateSubscription, store::Store};

/// Resolve a subscription URL into a list of (display_name, raw_url, source) template files.
/// Supports GitHub repositories (tree/recursive listing via the contents API) and
/// direct file URLs. Returns up to `limit` entries.
pub async fn discover_templates(
    client: &Client,
    url: &str,
    limit: usize,
) -> Result<Vec<(String, String, String)>> {
    if let Some((owner, repo, branch)) = parse_github_url(url) {
        discover_github(client, &owner, &repo, &branch, limit).await
    } else {
        // Treat as a direct file URL.
        let name = url
            .rsplit('/')
            .next()
            .unwrap_or("template")
            .trim_end_matches(".json")
            .trim_end_matches(".har")
            .to_string();
        Ok(vec![(name, url.to_string(), url.to_string())])
    }
}

fn parse_github_url(url: &str) -> Option<(String, String, String)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let owner = parts.first()?.to_string();
    let repo = parts.get(1)?.trim_end_matches(".git").to_string();
    parts.drain(0..2);
    let mut branch = "HEAD".to_string();
    if let Some(first) = parts.first().copied()
        && (first == "tree" || first == "blob")
    {
        parts.remove(0);
        if let Some(b) = parts.first().copied() {
            branch = b.to_string();
            parts.remove(0);
        }
    }
    let _ = parts; // remaining path prefix is currently ignored; we scan the whole tree
    Some((owner, repo, branch))
}

async fn discover_github(
    client: &Client,
    owner: &str,
    repo: &str,
    branch: &str,
    limit: usize,
) -> Result<Vec<(String, String, String)>> {
    let api_url =
        format!("https://api.github.com/repos/{owner}/{repo}/git/trees/{branch}?recursive=1");
    let response = client
        .get(&api_url)
        .header("User-Agent", "qdrust-subscription")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("cannot reach GitHub API")?
        .error_for_status()
        .context("GitHub API returned an error")?;
    let body: Value = response
        .json()
        .await
        .context("invalid GitHub API response")?;
    let Some(tree) = body.get("tree").and_then(Value::as_array) else {
        bail!("GitHub tree response has no entries");
    };
    let mut found = Vec::new();
    for entry in tree {
        if found.len() >= limit {
            break;
        }
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("blob") {
            continue;
        }
        if !looks_like_qd_template(path) {
            continue;
        }
        let raw_url = format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{path}");
        let name = file_stem(path).to_string();
        found.push((
            name,
            raw_url,
            format!("https://github.com/{owner}/{repo}/blob/{branch}/{path}"),
        ));
    }
    Ok(found)
}

fn looks_like_qd_template(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".har.json") || (lower.ends_with(".json") && !lower.ends_with(".har.json"))
}

fn file_stem(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.trim_end_matches(".har.json").trim_end_matches(".json")
}

/// Run a subscription sync: discover templates in the source, download each,
/// import or update the matching template, and report progress.
pub async fn sync_subscription(
    store: &Store,
    client: &Client,
    subscription: &TemplateSubscription,
    events: Option<broadcast::Sender<Value>>,
) -> Result<()> {
    let sync = store
        .create_subscription_sync(subscription.id)
        .await
        .context("cannot create sync record")?;
    let sync_id = sync.id;
    emit(events.as_ref(), subscription.id, sync_id, "started", None);
    let started = std::time::Instant::now();
    let result =
        sync_subscription_inner(store, client, subscription, events.as_ref(), sync_id).await;
    match result {
        Ok(imported) => {
            store
                .finish_subscription_sync(
                    sync_id,
                    "succeeded",
                    Some(&format!("imported {imported} template(s)")),
                )
                .await
                .ok();
            store
                .mark_subscription_synced(subscription.id, subscription.owner_id, None)
                .await
                .ok();
            emit(
                events.as_ref(),
                subscription.id,
                sync_id,
                "succeeded",
                Some(format!("imported {imported} template(s)")),
            );
            info!(
                subscription_id = subscription.id,
                imported,
                elapsed_ms = started.elapsed().as_millis(),
                "subscription sync completed"
            );
            Ok(())
        }
        Err(err) => {
            let message = bounded(&err.to_string());
            store
                .finish_subscription_sync(sync_id, "failed", Some(&message))
                .await
                .ok();
            store
                .mark_subscription_synced(subscription.id, subscription.owner_id, Some(&message))
                .await
                .ok();
            emit(
                events.as_ref(),
                subscription.id,
                sync_id,
                "failed",
                Some(message.clone()),
            );
            warn!(
                subscription_id = subscription.id,
                error = %message,
                "subscription sync failed"
            );
            Err(anyhow!("subscription sync failed: {message}"))
        }
    }
}

async fn sync_subscription_inner(
    store: &Store,
    client: &Client,
    subscription: &TemplateSubscription,
    events: Option<&broadcast::Sender<Value>>,
    sync_id: i64,
) -> Result<usize> {
    store
        .finish_subscription_sync(sync_id, "running", None)
        .await?;
    if !subscription.enabled {
        bail!("subscription is disabled");
    }
    let files = discover_templates(client, &subscription.url, 200).await?;
    if files.is_empty() {
        bail!("no template files found in subscription source");
    }
    emit(
        events,
        subscription.id,
        sync_id,
        "progress",
        Some(format!("found {} template file(s)", files.len())),
    );
    let mut imported = 0_usize;
    for (index, (name, raw_url, source)) in files.iter().enumerate() {
        emit(
            events,
            subscription.id,
            sync_id,
            "progress",
            Some(format!(
                "[{}/{}] downloading {name}",
                index + 1,
                files.len()
            )),
        );
        let response = client
            .get(raw_url)
            .header("User-Agent", "qdrust-subscription")
            .send()
            .await
            .with_context(|| format!("cannot download {name}"))?
            .error_for_status()
            .with_context(|| format!("download failed for {name}"))?;
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("cannot read body of {name}"))?;
        let har: Value =
            serde_json::from_slice(&bytes).with_context(|| format!("{name} is not valid JSON"))?;
        QdHar::parse_qd(har.clone()).with_context(|| format!("{name} is not a valid QD HAR"))?;
        upsert_subscription_template(store, subscription.owner_id, name, har, source).await?;
        imported += 1;
    }
    Ok(imported)
}

async fn upsert_subscription_template(
    store: &Store,
    owner_id: i64,
    name: &str,
    har: Value,
    _source: &str,
) -> Result<()> {
    let existing: Option<i64> = store.find_template_by_name(owner_id, name).await?;
    match existing {
        Some(id) => {
            store
                .update_qd_har_for_owner(
                    id,
                    owner_id,
                    crate::model::UpdateQdHarTemplate {
                        name: name.to_string(),
                        description: None,
                        har,
                    },
                )
                .await?;
        }
        None => {
            store
                .import_qd_har_for_owner(
                    owner_id,
                    crate::model::ImportQdHarTemplate {
                        name: name.to_string(),
                        description: None,
                        har,
                    },
                )
                .await?;
        }
    }
    Ok(())
}

fn emit(
    events: Option<&broadcast::Sender<Value>>,
    subscription_id: i64,
    sync_id: i64,
    kind: &str,
    message: Option<String>,
) {
    if let Some(events) = events {
        let _ = events.send(json!({
            "type": kind,
            "subscription_id": subscription_id,
            "sync_id": sync_id,
            "message": message,
        }));
    }
}

fn bounded(message: &str) -> String {
    const MAX: usize = 4096;
    let mut bounded = message.chars().take(MAX).collect::<String>();
    if message.chars().count() > MAX {
        bounded.push_str("...");
    }
    bounded
}

/// Keep a small timeout guard for the whole sync (10 minutes max).
pub fn sync_timeout() -> Duration {
    Duration::from_secs(600)
}
