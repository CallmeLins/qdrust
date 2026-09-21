//! Every outbound HTTP request the server makes goes through here.
//!
//! There used to be one bare client, built in `main` with a timeout and nothing
//! else and stored on the router state and in the scheduler. Four paths used it
//! — a task with a plain URL, a notification channel, a channel test, a library
//! subscription — and none of them went through the SSRF guard the executor
//! applies, so the ADR-0008 switches reached template runs and nothing else.
//! The ungated set included the most ordinary path of all, "fetch this URL on a
//! schedule", which is what the feature is for.
//!
//! This type exists to keep that from coming back: it is the crate's only way
//! to build a request, and it reads the settings on every call, so no caller
//! can hold a client that outlived the switch it was built under. The
//! `no_bare_client_outside_this_module` test holds the crate to it.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use qdrust_core::executor::{OutboundPolicy, guarded_client_for_url};
use reqwest::{Method, RequestBuilder};

use crate::api::RuntimeSettings;

/// What the guard says when it refuses a target. Matched inside the error chain
/// to add the one thing the guard cannot know: that an administrator can lift
/// the refusal, and which setting does it.
const BLOCKED: &str = "private or special-use network target is blocked";

/// The server's outbound client.
///
/// `Clone` is cheap and shares the settings, so every holder sees the same
/// switches.
#[derive(Clone)]
pub struct OutboundHttp {
    settings: Arc<RwLock<RuntimeSettings>>,
    /// Timeout for a request that does not set its own with
    /// `RequestBuilder::timeout`. Same value the bare client carried, so a
    /// path that never opted into a different timeout keeps its pace.
    timeout: Duration,
}

impl OutboundHttp {
    pub fn new(settings: Arc<RwLock<RuntimeSettings>>, timeout: Duration) -> Self {
        Self { settings, timeout }
    }

    /// A client for code with no settings to read: the `router()` convenience
    /// constructor and tests. Defaults to a closed posture — every relaxation
    /// off — which is also what a fresh [`RuntimeSettings`] holds.
    pub fn standalone() -> Self {
        Self::new(crate::api::runtime_settings(), Duration::from_secs(30))
    }

    /// The posture for one request, read as the settings are now.
    ///
    /// Per request rather than once at construction, for the same reason the
    /// scheduler reads its policy per run: an admin switch that needs a restart
    /// reads as a switch that does not work, and one that keeps granting access
    /// for a while after being turned off reads as a security bug.
    fn policy(&self) -> OutboundPolicy {
        let settings = self.settings.read().unwrap();
        OutboundPolicy {
            timeout: self.timeout,
            allow_private_network: settings.allow_private_network,
            allow_invalid_certificates: settings.allow_invalid_certificates,
            // The server has no per-request proxy; the environment is left to
            // reqwest, which is what the bare client did too.
            proxy: None,
        }
    }

    pub async fn get(&self, url: &str) -> Result<RequestBuilder> {
        self.request(Method::GET, url).await
    }

    pub async fn post(&self, url: &str) -> Result<RequestBuilder> {
        self.request(Method::POST, url).await
    }

    /// Resolve `url`, check the answer against the policy, and hand back a
    /// request bound to a client pinned to that answer.
    ///
    /// Fallible on purpose: the failure has to surface where the request is
    /// built, not as a silent fallback to an unguarded client.
    pub async fn request(&self, method: Method, url: &str) -> Result<RequestBuilder> {
        let policy = self.policy();
        let client = guarded_client_for_url(url, &policy, None)
            .await
            .map_err(|err| explain_refusal(url, policy.allow_private_network, err))?;
        Ok(client.request(method, url))
    }
}

/// Point at the switch when the guard is what said no.
///
/// The refusal reaches a user through "send a test message", where "private or
/// special-use network target is blocked" is accurate and still leaves them
/// with nowhere to go: the setting that lifts it is two pages away. Anything
/// else keeps the message it came with, so a typo in a channel URL is not
/// answered with a lecture about networks.
fn explain_refusal(url: &str, private_allowed: bool, err: anyhow::Error) -> anyhow::Error {
    if !private_allowed && format!("{err:#}").contains(BLOCKED) {
        return err.context(format!(
            "{url} is on a private or special-use network, which the outbound policy blocks; an \
             administrator can allow it with the {key} setting",
            key = crate::api::ALLOW_PRIVATE_NETWORK_SETTING
        ));
    }
    err.context(format!("cannot reach {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bound-but-unconnected loopback port. The guard classifies the address
    /// before anything is sent, so nothing has to be listening.
    fn loopback_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    }

    fn settings() -> Arc<RwLock<RuntimeSettings>> {
        crate::api::runtime_settings()
    }

    #[tokio::test]
    async fn a_loopback_target_is_refused_and_the_error_names_the_switch() {
        // The blocked message comes from the guard, which cannot know that an
        // administrator can lift it. Without the added context the user is told
        // what happened and not what to do about it.
        // `{:#}` prints the whole chain. The hint this module adds is the
        // outermost message and the guard's own reason is its cause, so `{}`
        // alone would show the hint and hide the reason.
        let error = format!(
            "{:#}",
            OutboundHttp::standalone()
                .get(&loopback_url())
                .await
                .unwrap_err()
        );
        assert!(
            error.contains(BLOCKED),
            "the guard's own reason must survive: {error}"
        );
        assert!(
            error.contains(crate::api::ALLOW_PRIVATE_NETWORK_SETTING),
            "the refusal must name the setting that lifts it: {error}"
        );
    }

    #[tokio::test]
    async fn the_switch_is_read_per_request_not_at_construction() {
        // The regression this guards: a client built once at startup, which
        // would compile, look right and need a restart to notice the switch.
        let settings = settings();
        let outbound = OutboundHttp::new(settings.clone(), Duration::from_secs(5));
        let url = loopback_url();

        let refused = outbound
            .get(&url)
            .await
            .expect_err("off by default, the request must not be built");
        assert!(format!("{refused:#}").contains(BLOCKED));
        settings.write().unwrap().allow_private_network = true;
        // Bound rather than sent: the client is what this asserts, and a
        // `RequestBuilder` starts an idle connection the moment it is dropped.
        let _request = outbound
            .get(&url)
            .await
            .expect("the same handle must pick the switch up");
    }

    #[tokio::test]
    async fn a_failure_that_is_not_the_guard_keeps_its_own_message() {
        // A malformed URL must not be answered with advice about networks: the
        // added context is for the one failure it explains.
        let error = format!(
            "{:#}",
            OutboundHttp::standalone()
                .get("not a url")
                .await
                .unwrap_err()
        );
        assert!(
            !error.contains(crate::api::ALLOW_PRIVATE_NETWORK_SETTING),
            "an unrelated failure must not point at the switch: {error}"
        );
        assert!(error.contains("invalid rendered URL"), "{error}");
    }

    #[test]
    fn the_policy_carries_both_switches_and_the_configured_timeout() {
        // The certificate switch has no other way of reaching a notification
        // channel or a library fetch, and a dropped field here would present as
        // "the switch does nothing on this path" rather than as an error.
        let settings = settings();
        let outbound = OutboundHttp::new(settings.clone(), Duration::from_secs(7));
        assert_eq!(outbound.policy().timeout, Duration::from_secs(7));
        assert!(!outbound.policy().allow_private_network);
        assert!(!outbound.policy().allow_invalid_certificates);

        {
            let mut runtime = settings.write().unwrap();
            runtime.allow_private_network = true;
            runtime.allow_invalid_certificates = true;
        }
        assert!(outbound.policy().allow_private_network);
        assert!(outbound.policy().allow_invalid_certificates);
    }

    /// No bare client anywhere else in the crate.
    ///
    /// The point of the module is that an unguarded request is not merely
    /// absent but unavailable; a new `reqwest::Client::builder()` somewhere else
    /// would quietly reopen one of the four paths. Checked by text because Rust
    /// has no way to forbid a type in a module.
    ///
    /// `oidc.rs` is the documented exception: its client talks to the identity
    /// provider named in the deploy-time configuration, not to a URL a user
    /// supplies, and it predates this policy. It is listed here so that the
    /// exception is visible rather than implied.
    #[test]
    fn no_bare_client_outside_this_module() {
        let guarded = [
            ("api.rs", include_str!("api.rs")),
            ("scheduler.rs", include_str!("scheduler.rs")),
            ("library.rs", include_str!("library.rs")),
            ("delivery.rs", include_str!("delivery.rs")),
            ("push_channels.rs", include_str!("push_channels.rs")),
            ("main.rs", include_str!("main.rs")),
        ];
        let needle = ["reqwest::", "Client"].concat();
        for (name, source) in guarded {
            assert!(
                !source.contains(&needle),
                "{name} builds its own client; every outbound request goes through OutboundHttp"
            );
        }
        let oidc = include_str!("oidc.rs");
        assert!(
            oidc.contains(&needle),
            "oidc.rs is the documented exception; if it no longer builds its own client, drop it \
             from this list instead of leaving a stale exemption"
        );
    }

    #[test]
    fn the_policy_defaults_to_closed() {
        // The posture every setting has to argue its way out of.
        let policy = OutboundPolicy::default();
        assert!(!policy.allow_private_network);
        assert!(!policy.allow_invalid_certificates);
        assert!(policy.proxy.is_none());
    }

    #[tokio::test]
    async fn the_reason_matched_here_is_the_one_the_guard_produces() {
        // `BLOCKED` is a hand-copied string. If the guard rewords itself the
        // pointer at the switch stops being added, and the refusal reads as an
        // ordinary unreachable host — a silence, not a failure. Pinned against
        // the guard instead of against the copy.
        let url = loopback_url();
        let error = guarded_client_for_url(&url, &OutboundPolicy::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), BLOCKED);
    }
}
