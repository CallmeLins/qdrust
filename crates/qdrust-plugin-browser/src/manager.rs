//! Long-lived in-process manager of headless-browser sessions.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, ensure};
use chromiumoxide::page::Page;
use tokio::sync::{Mutex, OnceCell};
use tracing::{info, warn};

/// Default TTL after which an idle session is reclaimed.
pub const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(30 * 60);
/// Hard cap on a session's lifetime regardless of activity.
pub const DEFAULT_MAX_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// Upper bound on concurrently live sessions.
pub const DEFAULT_MAX_SESSIONS: usize = 16;
/// Interval at which the idle sweep re-checks sessions.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// A single live browser tab owned by the manager.
pub(crate) struct Session {
    page: Page,
    /// Serialises operations on a session. A wizard is sequential, but two
    /// runs must not interleave keystrokes on the same tab.
    lock: Mutex<()>,
    created_at: Instant,
    last_used_at: Arc<tokio::sync::watch::Sender<Instant>>,
}

impl Session {
    fn new(page: Page) -> Self {
        let created_at = Instant::now();
        let last_used_at = Arc::new(tokio::sync::watch::channel(created_at).0);
        Self {
            page,
            lock: Mutex::new(()),
            created_at,
            last_used_at,
        }
    }

    fn touch(&self) {
        let _ = self.last_used_at.send(Instant::now());
    }

    fn last_used(&self) -> Instant {
        *self.last_used_at.borrow()
    }
}

/// Long-lived in-process manager of headless-browser sessions.
///
/// `Send + Sync` so it can be shared behind `Arc` across the executor's
/// concurrent plugin calls. The chromiumoxide [`chromiumoxide::browser::Browser`]
/// is not `Sync` (it holds a `Child`), so it lives behind a short-lived mutex
/// and is only touched to open new tabs; every per-tab interaction goes through
/// the `Page`, which *is* `Sync`, so it needs no `Browser` lock.
pub struct BrowserSessionManager {
    endpoint: String,
    browser: Mutex<Option<chromiumoxide::browser::Browser>>,
    connected: OnceCell<()>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    handler: OnceCell<tokio::task::JoinHandle<()>>,
    sweep: OnceCell<tokio::task::JoinHandle<()>>,
    idle_ttl: Duration,
    max_lifetime: Duration,
    max_sessions: usize,
}

impl BrowserSessionManager {
    /// Build a manager. When `endpoint` is `None` (no `QDRUST_BROWSER_URL`),
    /// no browser is opened and every action fails loudly with a "not
    /// configured" error rather than silently returning nothing.
    pub fn new(endpoint: Option<String>) -> Self {
        Self {
            endpoint: endpoint.unwrap_or_default(),
            browser: Mutex::new(None),
            connected: OnceCell::new(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            handler: OnceCell::new(),
            sweep: OnceCell::new(),
            idle_ttl: DEFAULT_IDLE_TTL,
            max_lifetime: DEFAULT_MAX_LIFETIME,
            max_sessions: DEFAULT_MAX_SESSIONS,
        }
    }

    /// Build a manager from the `QDRUST_BROWSER_URL` environment variable,
    /// returning `None` when it is unset or blank.
    pub fn from_env() -> Option<Self> {
        std::env::var("QDRUST_BROWSER_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|url| Self::new(Some(url)))
    }

    /// Whether this manager is configured with a browser endpoint.
    pub fn is_configured(&self) -> bool {
        !self.endpoint.is_empty()
    }

    /// The configured CDP endpoint (may be empty when unconfigured).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Open the CDP connection once and start the handler/sweep tasks.
    ///
    /// Idempotent and safe under `&self`: connection state is guarded by an
    /// internal mutex so the first concurrent caller connects and the rest
    /// share the same browser.
    pub async fn ensure_connected(&self) -> Result<()> {
        if self.connected.get().is_some() {
            return Ok(());
        }
        let mut browser_guard = self.browser.lock().await;
        if browser_guard.is_some() {
            let _ = self.connected.set(());
            return Ok(());
        }
        ensure!(
            self.is_configured(),
            "browser is not configured: set QDRUST_BROWSER_URL"
        );
        info!(endpoint = %self.endpoint, "connecting headless browser");
        let (browser, mut handler) =
            chromiumoxide::browser::Browser::connect(self.endpoint.clone())
                .await
                .context("cannot connect to browser endpoint")?;
        // Drive the websocket event loop on a background task for the life of
        // the manager (unlike the one-shot subprocess CLI, this must stay alive
        // across many calls).
        let handler_task = tokio::spawn(async move {
            while let Some(event) = futures_util::StreamExt::next(&mut handler).await {
                if event.is_err() {
                    break;
                }
            }
        });
        let _ = self.connected.set(());
        let _ = self.handler.set(handler_task);
        self.spawn_sweep();
        *browser_guard = Some(browser);
        Ok(())
    }

    fn spawn_sweep(&self) {
        let sessions = self.sessions.clone();
        let idle_ttl = self.idle_ttl;
        let max_lifetime = self.max_lifetime;
        let sweep_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(SWEEP_INTERVAL);
            loop {
                interval.tick().await;
                let mut map = sessions.lock().await;
                let now = Instant::now();
                let dead: Vec<String> = map
                    .iter()
                    .filter(|(_, s)| {
                        now.duration_since(s.last_used()) > idle_ttl
                            || now.duration_since(s.created_at) > max_lifetime
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in dead {
                    if let Some(s) = map.remove(&id) {
                        let _ = s.page.close().await;
                        warn!(session = %id, "reclaimed idle browser session");
                    }
                }
            }
        });
        let _ = self.sweep.set(sweep_task);
    }

    /// Start a new session, navigating to `url`. Returns a fresh session id.
    pub async fn start(&self, url: &str) -> Result<String> {
        self.ensure_connected().await?;
        {
            let guard = self.sessions.lock().await;
            ensure!(
                guard.len() < self.max_sessions,
                "too many browser sessions (max {})",
                self.max_sessions
            );
        }
        // Create the tab under the browser lock, releasing it before navigation
        // so `goto` runs without holding the browser mutex.
        let page = {
            let browser_guard = self.browser.lock().await;
            let browser = browser_guard.as_ref().context("browser not connected")?;
            browser
                .new_page("about:blank")
                .await
                .context("cannot create a browser page")?
        };
        page.goto(url)
            .await
            .context("cannot navigate session to url")?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut guard = self.sessions.lock().await;
        guard.insert(id.clone(), Session::new(page));
        info!(session = %id, url = %url, "started browser session");
        Ok(id)
    }

    /// End a session and free its tab.
    pub async fn end(&self, session: &str) -> Result<()> {
        let mut guard = self.sessions.lock().await;
        if let Some(s) = guard.remove(session) {
            let _ = s.page.close().await;
            info!(session = %session, "ended browser session");
            Ok(())
        } else {
            Err(anyhow!("browser session not found: {session}"))
        }
    }

    /// Run `f` against a session's page, refreshing its idle timestamp and
    /// serialising concurrent operations on the same tab.
    ///
    /// The closure receives an **owned** [`Page`] clone (cheap: `Page` is
    /// `Arc`-backed). Passing an owned handle rather than `&Page` avoids the
    /// classic higher-ranked lifetime error when the returned future borrows
    /// the closure's input reference.
    pub async fn with_session<F, Fut, T>(&self, session: &str, f: F) -> Result<T>
    where
        F: FnOnce(Page) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        T: Send + 'static,
    {
        let guard = self.sessions.lock().await;
        let s = guard
            .get(session)
            .ok_or_else(|| anyhow!("browser session not found: {session}"))?;
        s.touch();
        let _page_lock = s.lock.lock().await;
        let page = s.page.clone();
        // Keep the map guard held for the operation so `end`/`start` cannot
        // remove this session mid-action, and the per-session lock serialises
        // concurrent operations on the same tab.
        f(page).await
    }

    /// Reclaim every live session and stop background tasks (shutdown).
    pub async fn shutdown(&self) {
        let mut guard = self.sessions.lock().await;
        for (_, s) in guard.drain() {
            let _ = s.page.close().await;
        }
        if let Some(h) = self.handler.get() {
            h.abort();
        }
        if let Some(s) = self.sweep.get() {
            s.abort();
        }
    }

    /// Open a throwaway tab, navigate to `url`, run `f`, then close it. Used
    /// for actions invoked without a `session` id, matching the old one-shot
    /// subprocess semantics but reusing the long-lived browser connection.
    pub async fn run_ephemeral<F, Fut, T>(&self, url: &str, f: F) -> Result<T>
    where
        F: FnOnce(Page) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
        T: Send + 'static,
    {
        self.ensure_connected().await?;
        let page = {
            let browser_guard = self.browser.lock().await;
            let browser = browser_guard.as_ref().context("browser not connected")?;
            browser
                .new_page("about:blank")
                .await
                .context("cannot create a browser page")?
        };
        page.goto(url).await.context("cannot navigate to url")?;
        let outcome = f(page.clone()).await;
        let _ = page.close().await;
        outcome
    }
}
