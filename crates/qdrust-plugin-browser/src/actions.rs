//! Concrete browser actions, each operating on an already-connected page.
//!
//! Shared by two callers:
//! - the in-process session manager (a `Page` handed out by [`crate::BrowserSessionManager`]);
//! - the one-shot subprocess CLI (`src/main.rs`), which connects, opens a
//!   throwaway page, runs one action and exits.
//!
//! Every function takes a `&Page` so it can run under a session that must stay
//! alive, and never navigates away from the caller's chosen page.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chromiumoxide::{
    cdp::browser_protocol::{
        emulation::SetDeviceMetricsOverrideParams, page::CaptureScreenshotFormat,
    },
    page::ScreenshotParams,
};

/// Render the page and return its HTML (JS already executed).
pub async fn content(page: &chromiumoxide::page::Page) -> Result<String> {
    page.content().await.context("cannot read page content")
}

/// Evaluate a JS expression and return the JSON value.
pub async fn evaluate(page: &chromiumoxide::page::Page, expr: &str) -> Result<serde_json::Value> {
    let result = page
        .evaluate_expression(expr)
        .await
        .context("cannot evaluate expression")?;
    Ok(result.value().cloned().unwrap_or(serde_json::json!(null)))
}

/// Parameters controlling a screenshot, parsed from a request query.
#[derive(Clone, Debug, Default)]
pub struct CaptureOptions {
    pub full_page: bool,
    pub format: CaptureFormat,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub wait_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureFormat {
    #[default]
    Png,
    Jpeg,
}

impl CaptureFormat {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "jpeg" | "jpg" => Self::Jpeg,
            _ => Self::Png,
        }
    }
}

/// Capture a screenshot of the page.
pub async fn screenshot(
    page: &chromiumoxide::page::Page,
    params: &CaptureOptions,
) -> Result<Vec<u8>> {
    if let (Some(w), Some(h)) = (params.width, params.height) {
        let _ = page
            .execute(SetDeviceMetricsOverrideParams::new(w, h, 1.0, false))
            .await;
    }
    if params.wait_ms > 0 {
        tokio::time::sleep(Duration::from_millis(params.wait_ms)).await;
    }
    let format = match params.format {
        CaptureFormat::Png => CaptureScreenshotFormat::Png,
        CaptureFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
    };
    let bytes = page
        .screenshot(
            ScreenshotParams::builder()
                .full_page(params.full_page)
                .format(format)
                .build(),
        )
        .await
        .context("cannot capture screenshot")?;
    Ok(bytes)
}

/// Type `value` into the element matched by `selector`.
///
/// When `clear` is true the field is emptied first (Ctrl+A + Backspace so the
/// `input`/`change` events fire and modern SPA bindings update). When `submit`
/// is true an Enter key is sent afterwards (form submit).
pub async fn type_into(
    page: &chromiumoxide::page::Page,
    selector: &str,
    value: &str,
    clear: bool,
    submit: bool,
) -> Result<()> {
    let element = find_element(page, selector).await?;
    if clear {
        element
            .click()
            .await
            .context("cannot focus element to clear")?;
        element
            .press_key("Control+A")
            .await
            .context("cannot select element content")?;
        element
            .press_key("Backspace")
            .await
            .context("cannot clear element")?;
    }
    element
        .type_str(value)
        .await
        .context("cannot type into element")?;
    if submit {
        element
            .press_key("Enter")
            .await
            .context("cannot press Enter")?;
    }
    Ok(())
}

/// Click the element matched by `selector`. Optionally wait `wait_ms` and then
/// for an element matching `wait_selector` to appear.
pub async fn click(
    page: &chromiumoxide::page::Page,
    selector: &str,
    wait_ms: u64,
    wait_selector: Option<&str>,
) -> Result<()> {
    let element = find_element(page, selector).await?;
    element.click().await.context("cannot click element")?;
    if wait_ms > 0 {
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    }
    if let Some(sel) = wait_selector {
        wait_for_element(page, sel).await?;
    }
    Ok(())
}

/// Locate a single element by CSS selector, failing loudly if absent.
async fn find_element(
    page: &chromiumoxide::page::Page,
    selector: &str,
) -> Result<chromiumoxide::element::Element> {
    page.find_element(selector)
        .await
        .with_context(|| format!("element not found: {selector}"))
}

/// Poll until an element matching `selector` appears (with a bounded timeout).
async fn wait_for_element(page: &chromiumoxide::page::Page, selector: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        if page.find_element(selector).await.is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(anyhow!("timed out waiting for element: {selector}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
