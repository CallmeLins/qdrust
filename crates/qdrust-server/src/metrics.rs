//! A Prometheus text-exposition endpoint.
//!
//! Self-hosted deployments had no way to answer "is the queue backing up?" or
//! "are notifications failing?" without opening the UI and scrolling. The
//! numbers here are deliberately the ones that only exist inside the process:
//! run outcomes, the size of the queue, and delivery results. Everything else
//! (request latency, connection counts) is already observable from the reverse
//! proxy or the database.
//!
//! No client library: the exposition format is a handful of lines of text, and
//! a dependency that spawns a listener of its own would fight the existing
//! router. The metrics are computed per scrape with a few aggregate queries,
//! which is why there is no background collector to keep in sync.
//!
//! Exposure is opt-in, because the counts themselves are information about the
//! deployment. Two ways to turn it on, checked in this order:
//!
//! - `QDRUST_METRICS_TOKEN` set: `GET /metrics` requires
//!   `Authorization: Bearer <token>`;
//! - `QDRUST_METRICS=1`: the endpoint is open, for a trusted network or a
//!   sidecar that cannot authenticate.
//!
//! With neither, the route answers 404, exactly as if it did not exist.

use subtle::ConstantTimeEq;

/// How `/metrics` may be scraped, resolved once at startup.
#[derive(Clone, Debug, Default)]
pub struct MetricsConfig {
    /// Required bearer token, when set.
    token: Option<String>,
    /// Whether the endpoint is served at all.
    enabled: bool,
}

impl MetricsConfig {
    pub fn from_env() -> Self {
        let token = std::env::var("QDRUST_METRICS_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let forced = std::env::var("QDRUST_METRICS")
            .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes"))
            .unwrap_or(false);
        Self {
            enabled: forced || token.is_some(),
            token,
        }
    }

    /// Build one explicitly, for tests and for a caller that does not read the
    /// environment.
    pub fn new(token: Option<String>, enabled: bool) -> Self {
        let token = token.filter(|value| !value.is_empty());
        Self {
            enabled: enabled || token.is_some(),
            token,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether `headers` carry the credential this config requires.
    ///
    /// Compared in constant time: a scrape endpoint is not a place to leak a
    /// token one byte at a time through response timing.
    pub(crate) fn authorised(&self, headers: &axum::http::HeaderMap) -> bool {
        let Some(expected) = self.token.as_deref() else {
            return true;
        };
        let presented = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default();
        presented.as_bytes().ct_eq(expected.as_bytes()).into()
    }
}

/// Render a snapshot in the Prometheus text format.
///
/// Labels are emitted for every status the schema allows, including the ones at
/// zero: a series that disappears when nothing is failing makes rate() and
/// alerting rules behave worse than a flat zero does.
pub fn render(snapshot: &crate::model::MetricsSnapshot) -> String {
    let mut out = String::new();
    out.push_str("# HELP qdrust_build_info Build metadata; always 1.\n");
    out.push_str("# TYPE qdrust_build_info gauge\n");
    out.push_str(&format!(
        "qdrust_build_info{{version=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION")
    ));

    out.push_str("# HELP qdrust_runs_total Run records by final status.\n");
    out.push_str("# TYPE qdrust_runs_total gauge\n");
    for (status, count) in &snapshot.runs_by_status {
        out.push_str(&format!(
            "qdrust_runs_total{{status=\"{status}\"}} {count}\n"
        ));
    }

    out.push_str("# HELP qdrust_runs_active Runs queued or in flight right now.\n");
    out.push_str("# TYPE qdrust_runs_active gauge\n");
    out.push_str(&format!("qdrust_runs_active {}\n", snapshot.active_runs));

    out.push_str("# HELP qdrust_runs_oldest_pending_age_seconds Age of the run that has been waiting longest; 0 when the queue is empty.\n");
    out.push_str("# TYPE qdrust_runs_oldest_pending_age_seconds gauge\n");
    out.push_str(&format!(
        "qdrust_runs_oldest_pending_age_seconds {}\n",
        snapshot.oldest_pending_age_seconds
    ));

    out.push_str("# HELP qdrust_tasks_total Tasks by whether they are scheduled.\n");
    out.push_str("# TYPE qdrust_tasks_total gauge\n");
    out.push_str(&format!(
        "qdrust_tasks_total{{disabled=\"false\"}} {}\n",
        snapshot.tasks_enabled
    ));
    out.push_str(&format!(
        "qdrust_tasks_total{{disabled=\"true\"}} {}\n",
        snapshot.tasks_disabled
    ));

    out.push_str(
        "# HELP qdrust_notification_deliveries_total Recorded notification attempts by outcome.\n",
    );
    out.push_str("# TYPE qdrust_notification_deliveries_total gauge\n");
    for (status, count) in &snapshot.deliveries_by_status {
        out.push_str(&format!(
            "qdrust_notification_deliveries_total{{status=\"{status}\"}} {count}\n"
        ));
    }

    out.push_str("# HELP qdrust_users_total Accounts that still exist.\n");
    out.push_str("# TYPE qdrust_users_total gauge\n");
    out.push_str(&format!("qdrust_users_total {}\n", snapshot.users));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    #[test]
    fn a_token_protects_the_endpoint() {
        let config = MetricsConfig::new(Some("s3cret".into()), false);
        assert!(config.is_enabled());
        assert!(config.authorised(&headers("Bearer s3cret")));
        assert!(!config.authorised(&headers("Bearer wrong")));
        assert!(!config.authorised(&axum::http::HeaderMap::new()));
    }

    /// The switch alone exposes the numbers; the token is the alternative, not
    /// a prerequisite.
    #[test]
    fn the_switch_alone_enables_it_without_a_credential() {
        let config = MetricsConfig::new(None, true);
        assert!(config.is_enabled());
        assert!(config.authorised(&axum::http::HeaderMap::new()));
    }

    #[test]
    fn neither_switch_means_disabled() {
        let config = MetricsConfig::new(None, false);
        assert!(!config.is_enabled());
    }

    /// Every allowed status is a series even at zero, so dashboards and alerts
    /// do not have to special-case the first failure.
    #[test]
    fn renders_one_series_per_status() {
        let snapshot = crate::model::MetricsSnapshot {
            runs_by_status: vec![
                ("succeeded".into(), 12),
                ("failed".into(), 0),
                ("cancelled".into(), 0),
                ("pending".into(), 1),
                ("running".into(), 0),
            ],
            active_runs: 1,
            oldest_pending_age_seconds: 42,
            tasks_enabled: 3,
            tasks_disabled: 1,
            deliveries_by_status: vec![("succeeded".into(), 5), ("failed".into(), 0)],
            users: 2,
        };
        let text = render(&snapshot);
        assert!(text.contains("qdrust_runs_total{status=\"failed\"} 0\n"));
        assert!(text.contains("qdrust_runs_total{status=\"succeeded\"} 12\n"));
        assert!(text.contains("qdrust_tasks_total{disabled=\"true\"} 1\n"));
        assert!(text.contains("qdrust_notification_deliveries_total{status=\"failed\"} 0\n"));
        assert!(text.contains("qdrust_runs_oldest_pending_age_seconds 42\n"));
        assert!(text.contains(&format!(
            "qdrust_build_info{{version=\"{}\"}} 1\n",
            env!("CARGO_PKG_VERSION")
        )));
        // Every metric carries a HELP and a TYPE line, which is what the format
        // requires for a scraper to accept it.
        assert_eq!(text.matches("# HELP").count(), 7);
        assert_eq!(text.matches("# TYPE").count(), 7);
    }
}
