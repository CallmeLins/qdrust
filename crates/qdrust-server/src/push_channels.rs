//! QD-style push notification channel senders.
//!
//! Each sender mirrors the wire protocol used by the upstream qiandao
//! (QD) project: bark, ServerChan, Telegram, DingTalk robot, WxPusher
//! (app token + uid and SPT simple-push), WeCom Work Pusher
//! (application message) and WeCom group robot webhook.

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};

use crate::outbound::OutboundHttp;

/// Push channel kinds handled by this module (everything except
/// "webhook" and "email", which the scheduler delivers itself).
pub const CHANNEL_KINDS: [&str; 8] = [
    "bark",
    "serverchan",
    "telegram",
    "dingtalk",
    "wxpusher",
    "wxpusher_spt",
    "wecom_app",
    "wecom_webhook",
];

pub fn is_push_channel(kind: &str) -> bool {
    CHANNEL_KINDS.contains(&kind)
}

/// Every notification channel kind the server accepts: the three it delivers
/// itself (generic `webhook`, `custom_http`, `email`) plus the push channels
/// above. Kept in one place so the API pre-check and the config validator
/// cannot drift apart — a missing entry here is exactly how `custom_http`
/// once became impossible to create.
pub const ALL_CHANNEL_KINDS: [&str; 11] = [
    "webhook",
    "custom_http",
    "email",
    "bark",
    "serverchan",
    "telegram",
    "dingtalk",
    "wxpusher",
    "wxpusher_spt",
    "wecom_app",
    "wecom_webhook",
];

pub fn is_known_channel_kind(kind: &str) -> bool {
    ALL_CHANNEL_KINDS.contains(&kind)
}

/// Dispatch a rendered notification to the given channel kind.
pub async fn push_to_channel(
    outbound: &OutboundHttp,
    kind: &str,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    match kind {
        "bark" => send_bark(outbound, config, title, body).await,
        "serverchan" => send_serverchan(outbound, config, title, body).await,
        "telegram" => send_telegram(outbound, config, title, body).await,
        "dingtalk" => send_dingtalk(outbound, config, title, body).await,
        "wxpusher" => send_wxpusher(outbound, config, title, body).await,
        "wxpusher_spt" => send_wxpusher_spt(outbound, config, title, body).await,
        "wecom_app" => send_wecom_app(outbound, config, title, body).await,
        "wecom_webhook" => send_wecom_webhook(outbound, config, title, body).await,
        other => Err(anyhow!("unsupported push channel kind: {other}")),
    }
}

// ---------- helpers ----------

fn cfg_str<'a>(config: &'a Value, key: &str) -> Option<&'a str> {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn ensure_trailing_slash(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{url}/")
    }
}

fn ensure_scheme(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

fn plain_body(title: &str, body: &str) -> String {
    format!("{title}\n{body}")
}

/// Validate a JSON response where business success is signalled in the
/// payload rather than the HTTP status.
async fn check_business_code(
    response: reqwest::Response,
    code_field: &str,
    ok_value: i64,
) -> anyhow::Result<()> {
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .with_context(|| format!("cannot decode channel response (HTTP {status})"))?;
    let code = payload
        .get(code_field)
        .and_then(Value::as_i64)
        .unwrap_or(ok_value);
    if code != ok_value {
        bail!("channel returned {}: {}", code_field, payload);
    }
    Ok(())
}

// ---------- bark ----------

async fn send_bark(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let url = cfg_str(config, "url").ok_or_else(|| {
        anyhow!("bark channel requires a device URL (e.g. https://api.day.app/yourkey)")
    })?;
    let url = ensure_trailing_slash(url);
    let mut payload = json!({ "title": title, "body": body.replace("\\r\\n", "\n") });
    for key in ["sound", "group", "icon"] {
        if let Some(value) = cfg_str(config, key) {
            payload[key] = json!(value);
        }
    }
    outbound
        .post(&url)
        .await?
        .json(&payload)
        .send()
        .await
        .context("bark request failed")?
        .error_for_status()
        .context("bark rejected the notification")?;
    Ok(())
}

// ---------- ServerChan ----------

async fn send_serverchan(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let key = cfg_str(config, "sendkey")
        .ok_or_else(|| anyhow!("serverchan channel requires a SendKey"))?;
    let url = format!(
        "https://sctapi.ftqq.com/{}.send",
        key.trim_end_matches(".send")
    );
    outbound
        .post(&url)
        .await?
        .json(&json!({ "text": title, "desp": body.replace("\\r\\n", "\n\n") }))
        .send()
        .await
        .context("serverchan request failed")?
        .error_for_status()
        .context("serverchan rejected the notification")?;
    Ok(())
}

// ---------- Telegram ----------

async fn send_telegram(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let token =
        cfg_str(config, "token").ok_or_else(|| anyhow!("telegram channel requires a bot token"))?;
    let chat_id =
        cfg_str(config, "chat_id").ok_or_else(|| anyhow!("telegram channel requires a chat id"))?;
    let text = format!("<b>{}</b>\n{}", title, body.replace("\\r\\n", "\n"));
    let payload = json!({
        "chat_id": chat_id,
        "text": text,
        "disable_web_page_preview": true,
        "parse_mode": "HTML",
    });
    let url = match cfg_str(config, "host") {
        Some(host) => format!(
            "{}bot{}/sendMessage",
            ensure_trailing_slash(&ensure_scheme(host)),
            token
        ),
        None => format!("https://api.telegram.org/bot{token}/sendMessage"),
    };
    outbound
        .post(&url)
        .await?
        .json(&payload)
        .send()
        .await
        .context("telegram request failed")?
        .error_for_status()
        .context("telegram rejected the notification")?;
    Ok(())
}

// ---------- DingTalk robot ----------

async fn send_dingtalk(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let token = cfg_str(config, "access_token")
        .ok_or_else(|| anyhow!("dingtalk channel requires a robot access token"))?;
    let url = format!("https://oapi.dingtalk.com/robot/send?access_token={token}");
    let response = outbound
        .post(&url)
        .await?
        .json(&json!({
            "msgtype": "markdown",
            "markdown": {
                "title": title,
                "text": format!("#### {}\n\n{}", title, body.replace("\\r\\n", "\n\n")),
            }
        }))
        .send()
        .await
        .context("dingtalk request failed")?
        .error_for_status()
        .context("dingtalk rejected the notification")?;
    check_business_code(response, "errcode", 0).await
}

// ---------- WxPusher ----------

async fn send_wxpusher(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let app_token = cfg_str(config, "app_token")
        .ok_or_else(|| anyhow!("wxpusher channel requires an appToken"))?;
    let uid = cfg_str(config, "uid").ok_or_else(|| anyhow!("wxpusher channel requires a uid"))?;
    let content = plain_body(title, &body.replace("\\r\\n", "\n"));
    let response = outbound
        .post("https://wxpusher.zjiecode.com/api/send/message")
        .await?
        .json(&json!({
            "appToken": app_token,
            "content": content,
            "summary": content.chars().take(99).collect::<String>(),
            "contentType": 1,
            "uids": [uid],
        }))
        .send()
        .await
        .context("wxpusher request failed")?
        .error_for_status()
        .context("wxpusher rejected the notification")?;
    let payload: Value = response
        .json()
        .await
        .context("cannot decode wxpusher response")?;
    if payload.get("success").and_then(Value::as_bool) != Some(true) {
        bail!("wxpusher returned an error: {payload}");
    }
    Ok(())
}

async fn send_wxpusher_spt(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let raw = cfg_str(config, "spt")
        .ok_or_else(|| anyhow!("wxpusher_spt channel requires an SPT code"))?;
    let spts: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(10)
        .collect();
    if spts.is_empty() {
        bail!("wxpusher_spt channel requires at least one SPT code");
    }
    let content = plain_body(title, &body.replace("\\r\\n", "\n"));
    let response = outbound
        .post("https://wxpusher.zjiecode.com/api/send/message/simple-push")
        .await?
        .json(&json!({
            "content": content,
            "summary": content.chars().take(99).collect::<String>(),
            "contentType": 1,
            "sptList": spts,
        }))
        .send()
        .await
        .context("wxpusher request failed")?
        .error_for_status()
        .context("wxpusher rejected the notification")?;
    check_business_code(response, "code", 1000).await
}

// ---------- WeCom application pusher ----------

async fn send_wecom_app(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let corpid =
        cfg_str(config, "corpid").ok_or_else(|| anyhow!("wecom_app channel requires a corpid"))?;
    let secret =
        cfg_str(config, "secret").ok_or_else(|| anyhow!("wecom_app channel requires a secret"))?;
    let agentid = cfg_str(config, "agentid")
        .ok_or_else(|| anyhow!("wecom_app channel requires an agentid"))?;
    let to_user = cfg_str(config, "to_user").unwrap_or("@all");
    let base = match cfg_str(config, "proxy") {
        Some(proxy) => ensure_trailing_slash(&ensure_scheme(proxy)),
        None => "https://qyapi.weixin.qq.com/".into(),
    };

    let token_response: Value = outbound
        .get(&format!("{base}cgi-bin/gettoken"))
        .await?
        .query(&[("corpid", corpid), ("corpsecret", secret)])
        .send()
        .await
        .context("cannot fetch wecom access token")?
        .error_for_status()
        .context("wecom rejected the token request")?
        .json()
        .await
        .context("cannot decode wecom token response")?;
    let access_token = token_response
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("cannot fetch wecom access token: {token_response}"))?;

    // The WeCom API expects a numeric agentid; fall back to the raw
    // string when the stored value is not numeric.
    let agent_id_value = agentid
        .parse::<i64>()
        .map(|value| json!(value))
        .unwrap_or_else(|_| json!(agentid));

    let response = outbound
        .post(&format!(
            "{base}cgi-bin/message/send?access_token={access_token}"
        ))
        .await?
        .json(&json!({
            "touser": to_user,
            "msgtype": "text",
            "agentid": agent_id_value,
            "text": { "content": plain_body(title, &body.replace("\\r\\n", "\n")) },
        }))
        .send()
        .await
        .context("wecom message/send request failed")?
        .error_for_status()
        .context("wecom rejected the notification")?;
    check_business_code(response, "errcode", 0).await
}

// ---------- WeCom group robot webhook ----------

async fn send_wecom_webhook(
    outbound: &OutboundHttp,
    config: &Value,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    let key = cfg_str(config, "key")
        .ok_or_else(|| anyhow!("wecom_webhook channel requires a webhook key"))?;
    let response = outbound
        .post(&format!(
            "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key={key}"
        ))
        .await?
        .json(&json!({
            "msgtype": "text",
            "text": { "content": plain_body(title, &body.replace("\\r\\n", "\n")) },
        }))
        .send()
        .await
        .context("wecom webhook request failed")?
        .error_for_status()
        .context("wecom webhook rejected the notification")?;
    check_business_code(response, "errcode", 0).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn recognizes_push_kinds() {
        for kind in CHANNEL_KINDS {
            assert!(is_push_channel(kind), "{kind} should be a push channel");
        }
        assert!(!is_push_channel("webhook"));
        assert!(!is_push_channel("email"));
        assert!(!is_push_channel("unknown"));
    }

    #[test]
    fn recognizes_every_accepted_channel_kind() {
        // Both the API pre-check and the store's config validator key off this
        // list, so every deliverable kind must be present. A kind added to
        // CHANNEL_KINDS without a matching entry here is what made
        // `custom_http` un-creatable.
        for kind in CHANNEL_KINDS {
            assert!(is_known_channel_kind(kind), "{kind} missing");
        }
        for kind in ["webhook", "custom_http", "email"] {
            assert!(is_known_channel_kind(kind), "{kind} missing");
        }
        assert!(!is_known_channel_kind("nope"));
    }

    #[tokio::test]
    async fn rejects_missing_config() {
        let client = OutboundHttp::standalone();
        for kind in CHANNEL_KINDS {
            let result = push_to_channel(&client, kind, &json!({}), "t", "b").await;
            assert!(result.is_err(), "{kind} should fail without config");
        }
    }

    #[tokio::test]
    async fn rejects_unsupported_kind() {
        let client = OutboundHttp::standalone();
        let err = push_to_channel(&client, "nope", &json!({}), "t", "b")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[test]
    fn helpers_normalize_urls() {
        assert_eq!(ensure_trailing_slash("https://a.b/c"), "https://a.b/c/");
        assert_eq!(
            ensure_scheme("qyapi.weixin.qq.com/"),
            "https://qyapi.weixin.qq.com/"
        );
    }

    /// An `OutboundHttp` with the private-network switch set the way the test
    /// needs it. A fresh settings handle per call, so one test cannot leave a
    /// posture behind for another.
    fn guarded(allow_private_network: bool) -> OutboundHttp {
        let settings = crate::api::runtime_settings();
        settings.write().unwrap().allow_private_network = allow_private_network;
        OutboundHttp::new(settings, Duration::from_secs(30))
    }

    /// The push senders take the client as an argument instead of going through
    /// `deliver`'s two inline kinds, so a test on `webhook` alone would leave
    /// these eight free to build a client of their own and keep reaching a
    /// private address while the switch said no.
    ///
    /// The request count is the signal, and it has to be: 218 is neither a
    /// client nor a server error, so every sender here reports success whatever
    /// answered — a 200 from a proxy or a captive portal would look identical.
    /// A refused send must leave the count at zero and an allowed one must move
    /// it, which is a claim no other host on the network can satisfy.
    #[tokio::test]
    async fn a_push_channel_send_goes_through_the_same_guard() {
        let (address, served) = crate::test_support::serve_counting_loopback().await;
        let config = json!({ "url": format!("http://{address}/push") });

        let refused = push_to_channel(&guarded(false), "bark", &config, "t", "b")
            .await
            .unwrap_err();
        assert!(
            format!("{refused:#}").contains("private or special-use network target is blocked"),
            "a push channel on loopback must be refused while the switch is off: {refused:#}"
        );
        assert_eq!(
            served.load(Ordering::SeqCst),
            0,
            "a refused send must not reach the network at all"
        );

        push_to_channel(&guarded(true), "bark", &config, "t", "b")
            .await
            .expect("with the switch on the same channel must send");
        assert_eq!(
            served.load(Ordering::SeqCst),
            1,
            "the allowed send must have reached this socket"
        );
    }
}
