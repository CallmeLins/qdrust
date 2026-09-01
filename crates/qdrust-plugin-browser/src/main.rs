//! Subprocess plugin that drives a remote headless browser over CDP.
//!
//! Protocol follows the qdrust plugin contract: one JSON request line on
//! stdin, one JSON response envelope on stdout (see `PluginCallResponse`).
//!
//! The browser endpoint is taken from the `QDRUST_BROWSER_URL` environment
//! variable (set by the host) and can be overridden per call with a
//! `_browser_url` query parameter for ad-hoc debugging. Both an HTTP(S)
//! DevTools endpoint (local Chromium/obscura on :9222) and a WebSocket CDP
//! endpoint (Browserless, `ws://localhost:3000` or
//! `wss://chrome.browserless.io?token=...`) are accepted; `Browser::connect`
//! resolves the HTTP form by fetching `/json/version`.
//!
//! Actions:
//! - `content` — navigate to `url`, return the rendered page HTML.
//! - `eval` — navigate to `url`, run the JS `expr`, return the result as JSON
//!   (scalar / array / object / null).
//! - `screenshot` — navigate to `url`, capture a PNG/JPEG, return base64 in a
//!   `{"mimeType": ..., "data": ...}` JSON body. Optional `full_page`,
//!   `format` (png|jpeg), `width`/`height` (viewport), `wait` (ms delay before
//!   capture).
//!
//! Each invocation is a fresh subprocess and a fresh browser connection: it
//! connects, performs the single action, and exits. There is no session reuse
//! across calls - matching the one-shot `SubprocessPlugin` model.

use std::io::{self, BufRead};

use anyhow::Context;
use futures_util::StreamExt;
use qdrust_core::plugin::PluginRequest;
use serde_json::{Value, json};

/// Actions that need the network capability; the host enforces this is
/// declared in the plugin manifest.
const USED_CAPABILITIES: &[&str] = &["network"];

const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let line = io::stdin()
        .lock()
        .lines()
        .next()
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("missing plugin request"))?;
    let request: PluginRequest = serde_json::from_str(&line)?;
    let response = dispatch(&request).await;
    // Always emit a response envelope, even on error: the host reads stdout to
    // recover the (possibly non-200) response. A JSON error body with a 502
    // lets `success_asserts` / `failed_asserts` react instead of the host
    // failing on "invalid plugin response".
    let envelope = match response {
        Ok(response) => json!({
            "status": response.status,
            "headers": response.headers,
            "body": response.body,
            "capabilities_used": USED_CAPABILITIES,
        }),
        Err(err) => json!({
            "status": 502,
            "headers": {},
            "body": format!("browser plugin error: {err:#}").into_bytes(),
            "capabilities_used": USED_CAPABILITIES,
        }),
    };
    serde_json::to_writer(io::stdout(), &envelope)?;
    Ok(())
}

#[derive(Debug)]
struct Response {
    status: u16,
    headers: std::collections::BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn text(body: impl Into<String>) -> Self {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "text/plain; charset=UTF-8".to_string(),
        );
        Self {
            status: 200,
            headers,
            body: body.into().into_bytes(),
        }
    }

    fn json(value: &Value) -> Self {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/json; charset=UTF-8".to_string(),
        );
        Self {
            status: 200,
            headers,
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }
}

async fn dispatch(request: &PluginRequest) -> anyhow::Result<Response> {
    let browser_url = resolve_browser_url(request, |name| std::env::var(name).ok())?;
    match request.action.as_str() {
        "content" => {
            let url = required(request, "url")?.to_string();
            let html = run_browser(browser_url, |page| async move {
                page.goto(url).await?;
                let html = page.content().await?;
                anyhow::Ok(html)
            })
            .await?;
            Ok(Response::text(html))
        }
        "eval" => {
            let url = required(request, "url")?.to_string();
            let expr = request
                .query
                .get("expr")
                .or_else(|| request.query.get("expression"))
                .context("eval requires an expr parameter")?
                .clone();
            let value = run_browser(browser_url, |page| async move {
                page.goto(url).await?;
                let result = page.evaluate_expression(expr.as_str()).await?;
                let value = result.value().cloned().unwrap_or(json!(null));
                anyhow::Ok(value)
            })
            .await?;
            Ok(Response::json(&value))
        }
        "screenshot" => {
            let url = required(request, "url")?.to_string();
            let full_page = flag(request, "full_page");
            let format = request
                .query
                .get("format")
                .map(String::as_str)
                .unwrap_or("png");
            let wait_ms = request
                .query
                .get("wait")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let width = request
                .query
                .get("width")
                .and_then(|s| s.parse::<u32>().ok());
            let height = request
                .query
                .get("height")
                .and_then(|s| s.parse::<u32>().ok());

            let mime = match format {
                "jpeg" | "jpg" => "image/jpeg",
                _ => "image/png",
            };
            let png = run_browser(browser_url, move |page| async move {
                use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
                use chromiumoxide::page::ScreenshotParams;

                if let (Some(w), Some(h)) = (width, height) {
                    // Resize the viewport before navigating so the capture
                    // matches the requested dimensions.
                    let _ = page
                        .execute(
                            chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams::new(w, h, 1.0, false),
                        )
                        .await;
                }
                page.goto(url).await?;
                if wait_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                }
                let capture_format = if mime == "image/jpeg" {
                    CaptureScreenshotFormat::Jpeg
                } else {
                    CaptureScreenshotFormat::Png
                };
                let bytes = page
                    .screenshot(
                        ScreenshotParams::builder()
                            .full_page(full_page)
                            .format(capture_format)
                            .build(),
                    )
                    .await?;
                anyhow::Ok(bytes)
            })
            .await?;
            let base64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png);
            Ok(Response::json(&json!({
                "mimeType": mime,
                "data": base64,
            })))
        }
        other => anyhow::bail!("plugin action unavailable: browser/{other}"),
    }
}

/// Run a closure against a connected browser. Drives the CDP handler on a
/// background task, closes the browser afterwards, and enforces a hard timeout
/// so a wedged CDP endpoint cannot hang the subprocess forever (the host also
/// bounds this with `plugin_timeout`).
async fn run_browser<F, Fut, T>(browser_url: String, f: F) -> anyhow::Result<T>
where
    F: FnOnce(chromiumoxide::page::Page) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send,
{
    let (mut browser, mut handler) = chromiumoxide::browser::Browser::connect(browser_url)
        .await
        .context("cannot connect to browser endpoint")?;

    let handle = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    let page = browser
        .new_page("about:blank")
        .await
        .context("cannot create a browser page")?;

    let outcome = tokio::time::timeout(DEFAULT_TIMEOUT, f(page)).await;
    let _ = browser.close().await;
    handle.abort();

    outcome.map_err(|_| anyhow::anyhow!("browser operation timed out"))?
}

/// Resolve the browser endpoint: per-call `_browser_url` query param wins for
/// debugging, otherwise the `QDRUST_BROWSER_URL` env var set by the host.
fn resolve_browser_url(
    request: &PluginRequest,
    env_get: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<String> {
    if let Some(url) = request.query.get("_browser_url")
        && !url.trim().is_empty()
    {
        return Ok(url.clone());
    }
    env_get("QDRUST_BROWSER_URL")
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "browser is not configured: set QDRUST_BROWSER_URL env var (or pass _browser_url)"
            )
        })
}

fn required<'a>(request: &'a PluginRequest, name: &str) -> anyhow::Result<&'a str> {
    request
        .query
        .get(name)
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{name} parameter is required"))
}

/// Parse a truthy boolean query flag (empty/absent/`1`/`true`/`yes`).
fn flag(request: &PluginRequest, name: &str) -> bool {
    matches!(
        request.query.get(name).map(String::as_str).unwrap_or(""),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request(action: &str, pairs: &[(&str, &str)]) -> PluginRequest {
        PluginRequest {
            plugin_id: "browser".into(),
            action: action.into(),
            query: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn browser_url_prefers_query_param_over_env() {
        let req = request("content", &[("_browser_url", "ws://localhost:3000")]);
        let url =
            resolve_browser_url(&req, env_from(&[("QDRUST_BROWSER_URL", "http://x")])).unwrap();
        assert_eq!(url, "ws://localhost:3000");
    }

    #[test]
    fn browser_url_falls_back_to_env_when_query_missing() {
        let req = request("content", &[]);
        let url = resolve_browser_url(
            &req,
            env_from(&[("QDRUST_BROWSER_URL", "http://localhost:9222")]),
        )
        .unwrap();
        assert_eq!(url, "http://localhost:9222");
    }

    #[test]
    fn browser_url_ignores_blank_query_and_blank_env() {
        let req = request("content", &[("_browser_url", "  ")]);
        let err = resolve_browser_url(&req, env_from(&[("QDRUST_BROWSER_URL", "")])).unwrap_err();
        assert!(
            format!("{err:#}").contains("QDRUST_BROWSER_URL"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn browser_url_missing_both_reports_configuration_error() {
        let req = request("content", &[]);
        let err = resolve_browser_url(&req, env_from(&[])).unwrap_err();
        assert!(format!("{err:#}").contains("QDRUST_BROWSER_URL"), "{err:#}");
    }

    #[test]
    fn required_returns_value_or_errors() {
        let req = request("content", &[("url", "https://example.com")]);
        assert_eq!(required(&req, "url").unwrap(), "https://example.com");
        assert!(
            required(&req, "missing")
                .unwrap_err()
                .to_string()
                .contains("missing")
        );
        // blank counts as missing
        let blank = request("content", &[("url", "")]);
        assert!(required(&blank, "url").is_err());
    }

    #[test]
    fn flag_parses_truthy_values() {
        let req = request(
            "screenshot",
            &[("full_page", "1"), ("a", "true"), ("b", "yes")],
        );
        assert!(flag(&req, "full_page"));
        assert!(flag(&req, "a"));
        assert!(flag(&req, "b"));
        assert!(!flag(&req, "missing"));
        let off = request("screenshot", &[("full_page", "0")]);
        assert!(!flag(&off, "full_page"));
    }

    #[test]
    fn unknown_action_is_rejected() {
        // dispatch on an unknown action must fail before touching a browser.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let req = request("nope", &[("_browser_url", "ws://localhost:3000")]);
        let result = runtime.block_on(dispatch(&req));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("plugin action unavailable"),
            "unexpected error"
        );
    }
}
