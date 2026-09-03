//! Browser automation for qdrust.
//!
//! Hosts a long-lived headless-browser session manager that the qdrust server
//! runs **in-process** so DOM state survives across separate plugin calls (a
//! login page half-filled, a captcha image awaiting the user, an already
//! authenticated tab). Exposes a qdrust [`Plugin`] implementation registered
//! under id `browser`, reached from a template as `api://browser/<action>`.
//!
//! Design notes
//! ------------
//! - A single [`chromiumoxide::browser::Browser`] connection (obscura /
//!   Browserless) is kept for the life of the manager. Each "session" is one
//!   tab (`Page`) inside it, so multi-step interactions run on the same page
//!   across separate `call()`s.
//! - Sessions are created/ended explicitly (`start` / `end`) and additionally
//!   reclaimed by an idle/lifetime TTL sweep: a wizard that waits minutes for a
//!   human to read a captcha is not killed, yet abandoned tabs cannot leak.
//! - Sessions live in server memory; a process restart clears them (the wizard
//!   restarts with a fresh `start`).

pub mod actions;
pub mod manager;
pub mod plugin;

pub use manager::BrowserSessionManager;
pub use plugin::BrowserSessionPlugin;

/// Wire capabilities declared by the browser plugin manifest (connecting a
/// remote CDP endpoint is a network operation).
pub const DECLARED_CAPABILITIES: &[qdrust_core::plugin::PluginCapability] =
    &[qdrust_core::plugin::PluginCapability::Network];
