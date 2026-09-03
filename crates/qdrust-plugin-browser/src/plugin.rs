//! qdrust `Plugin` adapter that routes `api://browser/<action>` into a shared
//! [`BrowserSessionManager`].
//!
//! Session-aware actions (`start` / `end` / session-scoped `type` / `click` /
//! `screenshot`) let a wizard drive one live page across many calls. Actions
//! invoked without a `session` id fall back to a throwaway page (ephemeral),
//! matching the old one-shot subprocess semantics.

use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use qdrust_core::plugin::{PluginRequest, PluginResponse};
use serde_json::json;

use crate::{
    DECLARED_CAPABILITIES,
    actions::{
        CaptureFormat, CaptureOptions, click as action_click, content as action_content,
        evaluate as action_evaluate, screenshot as action_screenshot, type_into as action_type,
    },
    manager::BrowserSessionManager,
};

/// qdrust [`Plugin`] shim registered under id `browser`.
pub struct BrowserSessionPlugin {
    manager: Arc<BrowserSessionManager>,
    manifest: qdrust_core::plugin::PluginManifest,
}

impl BrowserSessionPlugin {
    pub fn new(manager: Arc<BrowserSessionManager>) -> Self {
        let manifest = qdrust_core::plugin::PluginManifest {
            api_version: qdrust_core::plugin::PLUGIN_API_VERSION,
            id: "browser".into(),
            name: "Browser automation".into(),
            version: "1".into(),
            capabilities: DECLARED_CAPABILITIES.to_vec(),
        };
        Self { manager, manifest }
    }

    /// Resolve the browser endpoint: per-call `_browser_url` query param wins
    /// for debugging, otherwise the manager's configured endpoint.
    fn browser_url<'a>(&self, request: &'a PluginRequest) -> Option<&'a str> {
        request
            .query
            .get("_browser_url")
            .filter(|v| !v.trim().is_empty())
            .map(String::as_str)
    }

    async fn dispatch(&self, request: &PluginRequest) -> Result<PluginResponse> {
        // When the caller passes a _browser_url override that differs from the
        // manager's configured endpoint, the shared connection is not usable.
        // For simplicity every in-process action uses the manager connection;
        // a mismatched override fails loudly rather than silently ignoring it.
        if let Some(override_url) = self.browser_url(request) {
            ensure_override_matches(override_url, self.manager.endpoint())?;
        }
        match request.action.as_str() {
            "start" => self.dispatch_start(request).await,
            "end" => self.dispatch_end(request).await,
            "content" => self.dispatch_content(request).await,
            "eval" => self.dispatch_eval(request).await,
            "screenshot" => self.dispatch_screenshot(request).await,
            "type" => self.dispatch_type(request).await,
            "click" => self.dispatch_click(request).await,
            "keepalive" => self.dispatch_keepalive(request).await,
            other => Err(anyhow!("plugin action unavailable: browser/{other}")),
        }
    }

    fn require<'a>(&self, request: &'a PluginRequest, name: &str) -> Result<&'a str> {
        request
            .query
            .get(name)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("{name} parameter is required"))
    }

    fn flag(&self, request: &PluginRequest, name: &str) -> bool {
        matches!(
            request.query.get(name).map(String::as_str).unwrap_or(""),
            "1" | "true" | "yes" | "on"
        )
    }

    fn session_id(&self, request: &PluginRequest) -> Option<String> {
        request
            .query
            .get("session")
            .or_else(|| request.query.get("session_id"))
            .filter(|v| !v.trim().is_empty())
            .cloned()
    }

    async fn dispatch_start(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let url = self.require(request, "url")?;
        let id = self.manager.start(url).await?;
        Ok(json_response(&json!({ "session": id })))
    }

    async fn dispatch_end(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let session = self
            .session_id(request)
            .ok_or_else(|| anyhow!("end requires a session id"))?;
        self.manager.end(&session).await?;
        Ok(json_response(
            &json!({ "session": session, "status": "closed" }),
        ))
    }

    async fn dispatch_content(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let url = self.require(request, "url")?.to_string();
        let html = self
            .page_action(
                request,
                url,
                |page| async move { action_content(&page).await },
            )
            .await?;
        Ok(text_response(html))
    }

    async fn dispatch_eval(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let url = self.require(request, "url")?.to_string();
        let expr = request
            .query
            .get("expr")
            .or_else(|| request.query.get("expression"))
            .context("eval requires an expr parameter")?
            .clone();
        let value = self
            .page_action(request, url, move |page| async move {
                action_evaluate(&page, &expr).await
            })
            .await?;
        Ok(json_response(&value))
    }

    async fn dispatch_screenshot(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let url = self.require(request, "url")?.to_string();
        let params = CaptureOptions {
            full_page: self.flag(request, "full_page"),
            format: CaptureFormat::parse(
                request
                    .query
                    .get("format")
                    .map(String::as_str)
                    .unwrap_or("png"),
            ),
            width: request
                .query
                .get("width")
                .and_then(|s| s.parse::<u32>().ok()),
            height: request
                .query
                .get("height")
                .and_then(|s| s.parse::<u32>().ok()),
            wait_ms: request
                .query
                .get("wait")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0),
        };
        let mime = params.format.mime();
        let png = self
            .page_action(request, url, move |page| async move {
                action_screenshot(&page, &params).await
            })
            .await?;
        let base64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png);
        Ok(json_response(&json!({ "mimeType": mime, "data": base64 })))
    }

    async fn dispatch_type(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let session = self.require(request, "session")?.to_string();
        let selector = self.require(request, "selector")?.to_string();
        let value = self.require(request, "value")?.to_string();
        let clear = self.flag(request, "clear");
        let submit = self.flag(request, "submit");
        let selector_c = selector.clone();
        let value_c = value.clone();
        self.manager
            .with_session(&session, move |page| async move {
                action_type(&page, &selector_c, &value_c, clear, submit).await
            })
            .await?;
        Ok(json_response(
            &json!({ "ok": true, "session": session, "selector": selector }),
        ))
    }

    async fn dispatch_click(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let session = self.require(request, "session")?.to_string();
        let selector = self.require(request, "selector")?.to_string();
        let wait_ms = request
            .query
            .get("wait")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let wait_selector = request.query.get("wait_selector").cloned();
        let selector_c = selector.clone();
        self.manager
            .with_session(&session, move |page| async move {
                action_click(&page, &selector_c, wait_ms, wait_selector.as_deref()).await
            })
            .await?;
        Ok(json_response(
            &json!({ "ok": true, "session": session, "selector": selector }),
        ))
    }

    async fn dispatch_keepalive(&self, request: &PluginRequest) -> Result<PluginResponse> {
        let session = self.require(request, "session")?.to_string();
        // Touch the session to refresh its idle TTL without any operation.
        self.manager
            .with_session(&session, |_page| async move { Ok(()) })
            .await?;
        Ok(json_response(&json!({ "ok": true, "session": session })))
    }

    /// Run an action against either the session's page or a throwaway page,
    /// depending on whether a `session` id is supplied.
    async fn page_action<F, Fut, T>(&self, request: &PluginRequest, url: String, f: F) -> Result<T>
    where
        F: FnOnce(chromiumoxide::page::Page) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        T: Send + 'static,
    {
        match self.session_id(request) {
            Some(session) => self.manager.with_session(&session, f).await,
            None => self.manager.run_ephemeral(&url, f).await,
        }
    }
}

fn ensure_override_matches(override_url: &str, configured: &str) -> Result<()> {
    if override_url == configured {
        return Ok(());
    }
    Err(anyhow!(
        "_browser_url override is not supported for in-process sessions; the \
         server manages one browser connection ({configured})"
    ))
}

fn json_response(value: &serde_json::Value) -> PluginResponse {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/json; charset=UTF-8".to_string(),
    );
    PluginResponse {
        status: 200,
        headers,
        body: serde_json::to_vec(value).unwrap_or_default(),
    }
}

fn text_response(body: String) -> PluginResponse {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "text/plain; charset=UTF-8".to_string(),
    );
    PluginResponse {
        status: 200,
        headers,
        body: body.into_bytes(),
    }
}

impl qdrust_core::plugin::Plugin for BrowserSessionPlugin {
    fn manifest(&self) -> &qdrust_core::plugin::PluginManifest {
        &self.manifest
    }

    fn call<'a>(
        &'a self,
        request: &'a PluginRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PluginResponse>> + Send + 'a>>
    {
        Box::pin(async move {
            // Every failure is folded into an Ok(502) response (not Err): the
            // template then flows through success/failed_asserts and can react
            // to the failed step instead of aborting the whole run. This
            // mirrors the one-shot subprocess plugin envelope semantics.
            match self.dispatch(request).await {
                Ok(response) => Ok(response),
                Err(err) => Ok(json_502(&err)),
            }
        })
    }
}

/// Build a 502 response carrying a textual error body.
fn json_502(err: &anyhow::Error) -> PluginResponse {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "text/plain; charset=UTF-8".to_string(),
    );
    PluginResponse {
        status: 502,
        headers,
        body: format!("browser plugin error: {err:#}").into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use qdrust_core::plugin::{Plugin, PluginRequest, PluginResponse};
    use std::collections::BTreeMap;

    use super::{BrowserSessionPlugin, json_502};
    use crate::BrowserSessionManager;

    /// Plugin backed by an *unconfigured* manager (no QDRUST_BROWSER_URL).
    /// Any action that needs to open/use a browser fails loudly here, so we can
    /// exercise dispatch + 502-folding without a real headless browser.
    fn plugin_unconfigured() -> BrowserSessionPlugin {
        BrowserSessionPlugin::new(Arc::new(BrowserSessionManager::new(None)))
    }

    fn plugin_at(endpoint: &str) -> BrowserSessionPlugin {
        BrowserSessionPlugin::new(Arc::new(BrowserSessionManager::new(Some(
            endpoint.to_string(),
        ))))
    }

    fn request(action: &str, query: &[(&str, &str)]) -> PluginRequest {
        PluginRequest {
            plugin_id: "browser".into(),
            action: action.into(),
            query: query
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn body_text(response: &PluginResponse) -> String {
        String::from_utf8_lossy(&response.body).to_string()
    }

    #[tokio::test]
    async fn manifest_declares_browser_id_and_network_capability() {
        let plugin = plugin_unconfigured();
        assert_eq!(plugin.manifest().id, "browser");
        assert_eq!(
            plugin.manifest().capabilities,
            vec![qdrust_core::plugin::PluginCapability::Network]
        );
    }

    #[tokio::test]
    async fn unconfigured_browser_action_folds_into_ok_502() {
        // content without a browser endpoint must surface as Ok(502), not Err,
        // so template asserts can react instead of aborting the whole run.
        let plugin = plugin_unconfigured();
        let response = plugin
            .call(&request("content", &[("url", "https://example.com")]))
            .await
            .expect("failures are folded into Ok");
        assert_eq!(response.status, 502);
        assert!(body_text(&response).contains("not configured"));
    }

    #[tokio::test]
    async fn missing_required_parameter_is_ok_502() {
        let plugin = plugin_unconfigured();
        let response = plugin
            .call(&request("eval", &[]))
            .await
            .expect("failures are folded into Ok");
        assert_eq!(response.status, 502);
        assert!(body_text(&response).contains("url parameter is required"));
    }

    #[tokio::test]
    async fn unknown_action_is_ok_502() {
        let plugin = plugin_unconfigured();
        let response = plugin
            .call(&request("nope", &[]))
            .await
            .expect("failures are folded into Ok");
        assert_eq!(response.status, 502);
        assert!(body_text(&response).contains("plugin action unavailable"));
    }

    #[tokio::test]
    async fn ending_a_missing_session_is_ok_502() {
        // A configured manager with no live sessions: end("does-not-exist")
        // reaches the manager and reports the session as missing.
        let plugin = plugin_at("ws://localhost:3000");
        let response = plugin
            .call(&request("end", &[("session", "does-not-exist")]))
            .await
            .expect("failures are folded into Ok");
        assert_eq!(response.status, 502);
        assert!(body_text(&response).contains("browser session not found"));
    }

    #[tokio::test]
    async fn session_actions_require_a_session_param() {
        let plugin = plugin_at("ws://localhost:3000");
        // type without a session -> missing required parameter -> 502.
        let response = plugin
            .call(&request(
                "type",
                &[("selector", "#user"), ("value", "alice")],
            ))
            .await
            .expect("failures are folded into Ok");
        assert_eq!(response.status, 502);
        assert!(body_text(&response).contains("session parameter is required"));
    }

    #[tokio::test]
    async fn mismatched_browser_url_override_fails_loudly() {
        // The manager is configured for one endpoint; a per-call override to a
        // different one is unusable and must be rejected, not silently ignored.
        let plugin = plugin_at("ws://localhost:3000");
        let response = plugin
            .call(&request(
                "content",
                &[
                    ("url", "https://example.com"),
                    ("_browser_url", "ws://elsewhere:3000"),
                ],
            ))
            .await
            .expect("failures are folded into Ok");
        assert_eq!(response.status, 502);
        assert!(body_text(&response).contains("override"));
    }

    #[tokio::test]
    async fn json_502_carries_a_text_error_body() {
        let err = anyhow::anyhow!("boom");
        let response = json_502(&err);
        assert_eq!(response.status, 502);
        assert!(body_text(&response).contains("boom"));
    }
}
