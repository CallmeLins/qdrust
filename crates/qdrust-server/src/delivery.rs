//! One delivery path for every notification channel kind.
//!
//! The scheduler used to inline the `webhook` / `custom_http` / `email` arms and
//! delegate the rest to [`crate::push_channels`]. The notification-channel
//! "send a test message" endpoint needs exactly the same behaviour, and a second
//! copy would drift — a channel that tests green but fails in production is
//! worse than no test at all. So both callers go through [`deliver`].

use anyhow::{Context, Result, anyhow};
use chrono::{TimeZone, Utc};
use reqwest::Method;
use serde_json::Value;

use crate::{
    email::{EmailClient, normalize_recipient},
    outbound::OutboundHttp,
};

/// Render an epoch-seconds timestamp in the task's own IANA timezone (UTC when
/// the task has none, or when the stored name no longer parses). Backs the
/// `{t}` variable in notification templates.
pub fn format_notification_time(timestamp: i64, timezone: Option<&str>) -> String {
    let tz = timezone
        .and_then(|tz| tz.parse::<chrono_tz::Tz>().ok())
        .unwrap_or(chrono_tz::Tz::UTC);
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map(|dt| {
            dt.with_timezone(&tz)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// Values substituted into a notification `title_template` / `body_template`
/// (and into a custom-HTTP url / headers / body). Kept unit-testable without a
/// store or a network client.
pub struct TemplateVars<'a> {
    pub event: &'a str,
    pub task_id: i64,
    pub task_name: &'a str,
    pub run_id: i64,
    pub status: String,
    pub error: &'a str,
    pub log: &'a str,
    pub time: String,
}

impl TemplateVars<'_> {
    /// Replace every known `{placeholder}`. Unknown placeholders are left as-is
    /// so a typo shows up in the delivered message instead of silently vanishing.
    pub fn render(&self, value: &str) -> String {
        value
            .replace("{event}", self.event)
            .replace("{task_id}", &self.task_id.to_string())
            .replace("{task}", self.task_name)
            .replace("{run_id}", &self.run_id.to_string())
            .replace("{status}", &self.status)
            .replace("{error}", self.error)
            .replace("{log}", self.log)
            .replace("{t}", &self.time)
    }
}

/// Placeholder values for a message that is not tied to a run, i.e. a channel
/// test triggered from the UI. Rendering real templates against these proves the
/// templates render as well as the credentials working.
pub fn test_vars(timezone: Option<&str>) -> TemplateVars<'static> {
    TemplateVars {
        event: "test",
        task_id: 0,
        task_name: "qdrust channel test",
        run_id: 0,
        status: "200".to_string(),
        error: "",
        log: "This is a test message from qdrust. If you can read this, the channel works.",
        time: format_notification_time(Utc::now().timestamp(), timezone),
    }
}

/// A message ready to hand to a channel.
pub struct Message<'a> {
    pub title: &'a str,
    pub body: &'a str,
    /// JSON body used by `webhook` / `custom_http` channels that have no body of
    /// their own configured.
    pub payload: &'a Value,
}

/// Deliver one already-rendered message through `kind` with `config`.
///
/// `render` substitutes `{var}` placeholders inside a custom-HTTP channel's url,
/// headers and configured body — the values differ between a finished run and a
/// channel test, so the caller supplies the substitution. It is `Send + Sync`
/// because the returned future is held by the scheduler's worker task.
///
/// `email` is only consulted for `email` channels; `None` builds an
/// environment-configured client instead (the API router carries no mail client).
/// A missing or malformed config yields an error rather than silence, so a
/// broken channel reports why.
///
/// The channel's URL is the user's to choose, so every request goes through
/// [`OutboundHttp`] — the same guard a template run gets. Turning the
/// private-network switch off closes this path too, which it did not before.
pub async fn deliver(
    outbound: &OutboundHttp,
    kind: &str,
    config: &Value,
    message: &Message<'_>,
    render: &(dyn Fn(&str) -> String + Send + Sync),
    email: Option<&EmailClient>,
) -> Result<()> {
    match kind {
        "webhook" => {
            let url = config
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("webhook channel requires a url"))?;
            outbound
                .post(url)
                .await?
                .json(message.payload)
                .send()
                .await
                .and_then(|response| response.error_for_status())
                .context("webhook delivery failed")?;
            Ok(())
        }
        "custom_http" => {
            let url = config
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("custom HTTP channel requires a url"))?;
            let method = config
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("POST");
            let method: Method = method
                .parse()
                .with_context(|| format!("custom HTTP channel has an invalid method: {method}"))?;
            // Rendered first: the guard has to see the URL that will actually be
            // requested, not the template with a `{run_id}` in the host.
            let mut request = outbound.request(method, &render(url)).await?;
            if let Some(headers) = config.get("headers").and_then(Value::as_object) {
                for (name, value) in headers {
                    if let Some(value) = value.as_str() {
                        request = request.header(name, render(value));
                    }
                }
            }
            let configured_body = config
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = if configured_body.is_empty() {
                request.json(message.payload).send().await
            } else {
                request.body(render(configured_body)).send().await
            };
            result
                .and_then(|response| response.error_for_status())
                .context("custom HTTP delivery failed")?;
            Ok(())
        }
        "email" => {
            let to = config
                .get("to")
                .and_then(Value::as_str)
                .and_then(normalize_recipient)
                .ok_or_else(|| anyhow!("email channel requires a valid recipient"))?;
            let from = config.get("from").and_then(Value::as_str);
            match email {
                Some(email) => email.send(&to, from, message.title, message.body),
                None => {
                    let client = EmailClient::new(crate::email::EmailConfig::from_env())
                        .context("cannot build the mail client for this email channel")?;
                    client.send(&to, from, message.title, message.body)
                }
            }
        }
        other if crate::push_channels::is_push_channel(other) => {
            crate::push_channels::push_to_channel(
                outbound,
                other,
                config,
                message.title,
                message.body,
            )
            .await
        }
        other => Err(anyhow!("unsupported notification channel kind: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn message<'a>(title: &'a str, body: &'a str, payload: &'a Value) -> Message<'a> {
        Message {
            title,
            body,
            payload,
        }
    }

    /// An `OutboundHttp` with the private-network switch set the way the test
    /// needs it. A fresh settings handle per call, so one test cannot leave a
    /// posture behind for another.
    fn guarded(allow_private_network: bool) -> OutboundHttp {
        let settings = crate::api::runtime_settings();
        settings.write().unwrap().allow_private_network = allow_private_network;
        OutboundHttp::new(settings, Duration::from_secs(30))
    }

    #[test]
    fn test_vars_feed_a_placeholder_template() {
        let vars = test_vars(Some("Asia/Shanghai"));
        assert_eq!(
            vars.render("[{event}] {task} #{task_id} status={status}"),
            "[test] qdrust channel test #0 status=200"
        );
        // `{t}` is the only variable that is not a constant: epoch 0 rendered in
        // the task timezone, so the test asserts the shape via the known value.
        assert_eq!(
            format_notification_time(0, Some("Asia/Shanghai")),
            "1970-01-01 08:00:00"
        );
        assert!(!vars.time.is_empty());
    }

    #[tokio::test]
    async fn unknown_channel_kind_is_reported_not_ignored() {
        let client = OutboundHttp::standalone();
        let payload = json!({});
        let err = deliver(
            &client,
            "carrier-pigeon",
            &json!({}),
            &message("t", "b", &payload),
            &|value| value.to_string(),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported notification channel kind: carrier-pigeon")
        );
    }

    #[tokio::test]
    async fn a_channel_missing_required_config_fails_loudly() {
        let client = OutboundHttp::standalone();
        let payload = json!({});
        let err = deliver(
            &client,
            "webhook",
            &json!({}),
            &message("t", "b", &payload),
            &|value| value.to_string(),
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("webhook channel requires a url"));
    }

    /// A channel URL is the user's to choose, and this path used to have no
    /// guard at all: the private-network switch closed template runs while a
    /// channel could still reach anything the host could.
    ///
    /// The request count is the assertion that matters, because delivery
    /// reports nothing but success or failure — the refusal has to leave the
    /// count at zero, which no status comparison could establish.
    #[tokio::test]
    async fn a_channel_url_goes_through_the_same_guard_as_a_template() {
        let (address, served) = crate::test_support::serve_counting_loopback().await;
        let config = json!({ "url": format!("http://{address}/hook") });
        let payload = json!({});

        let blocked = deliver(
            &guarded(false),
            "webhook",
            &config,
            &message("t", "b", &payload),
            &|value| value.to_string(),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            format!("{blocked:#}").contains("private or special-use network target is blocked"),
            "a channel pointing at loopback must be refused while the switch is off: {blocked:#}"
        );
        assert_eq!(
            served.load(Ordering::SeqCst),
            0,
            "a refused delivery must not reach the network at all"
        );

        deliver(
            &guarded(true),
            "webhook",
            &config,
            &message("t", "b", &payload),
            &|value| value.to_string(),
            None,
        )
        .await
        .expect("with the switch on the same channel must deliver");
        assert_eq!(
            served.load(Ordering::SeqCst),
            1,
            "the allowed delivery must have reached this socket"
        );
    }
}
