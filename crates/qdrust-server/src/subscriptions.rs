use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::{
    library,
    model::{SubscriptionMode, TemplateSubscription},
    store::Store,
};

/// Run a subscription sync: catalogue the source, download each entry, import
/// or update the matching template, and report progress.
///
/// Only `all`-mode subscriptions reach this; a `select`-mode source is a
/// library the user browses and imports from by hand (see `library`).
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
        Ok((imported, updated)) => {
            let summary = format!("imported {imported} template(s), updated {updated}");
            store
                .finish_subscription_sync(sync_id, "succeeded", Some(&summary))
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
                Some(summary),
            );
            info!(
                subscription_id = subscription.id,
                imported,
                updated,
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

/// Returns `(imported, updated)`.
async fn sync_subscription_inner(
    store: &Store,
    client: &Client,
    subscription: &TemplateSubscription,
    events: Option<&broadcast::Sender<Value>>,
    sync_id: i64,
) -> Result<(usize, usize)> {
    store
        .finish_subscription_sync(sync_id, "running", None)
        .await?;
    if !subscription.enabled {
        bail!("subscription is disabled");
    }
    if !matches!(
        SubscriptionMode::parse(&subscription.mode),
        Some(SubscriptionMode::All)
    ) {
        bail!("subscription is in select mode; import templates from the library instead");
    }
    let catalogue = library::catalogue(client, &subscription.url).await?;
    emit(
        events,
        subscription.id,
        sync_id,
        "progress",
        Some(format!(
            "found {} template(s) via {}",
            catalogue.entries.len(),
            catalogue.source_kind
        )),
    );
    let linked = library::installed_index(store.list_template_imports(subscription.id).await?);
    let source = library::parse_github_url(&subscription.url);
    let mut imported = 0_usize;
    let mut updated = 0_usize;
    let total = catalogue.entries.len();
    for (index, entry) in catalogue.entries.iter().enumerate() {
        let name = &entry.name;
        emit(
            events,
            subscription.id,
            sync_id,
            "progress",
            Some(format!("[{}/{}] downloading {name}", index + 1, total)),
        );
        // One unusable upstream template must not abort the whole sync; the
        // entry is skipped and surfaces in the log instead.
        match library::import_entry(
            store,
            client,
            subscription,
            source.as_ref(),
            entry,
            linked.get(name.as_str()).map(|import| import.template_id),
        )
        .await
        {
            Ok((_, true)) => updated += 1,
            Ok((_, false)) => imported += 1,
            Err(err) => warn!(
                subscription_id = subscription.id,
                entry = %name,
                %err,
                "skipping template that could not be imported"
            ),
        }
    }
    Ok((imported, updated))
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
