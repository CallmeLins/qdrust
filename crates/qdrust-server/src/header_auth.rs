//! Trusted reverse-proxy header authentication (Phase 4, forward-auth).
//!
//! A reverse proxy (authentik / authelia / nginx `auth_request`) that has
//! already authenticated the user injects identity headers (`Remote-User`,
//! `Remote-Email`, `Remote-Groups`, configurable) on every request. The server
//! only honours those headers when the request source IP is in the configured
//! `trusted_proxies` allow-list — otherwise a client could spoof them.
//!
//! This module is intentionally free of database / `AppState` concerns: it holds
//! the pure parsing and trust-decision logic plus unit tests, so the resolve
//! step (which needs the store) lives in `api.rs`.

use std::net::IpAddr;

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderName},
};

use crate::config::HeaderAuthConfig;

/// Parsed identity extracted from the configured headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderIdentity {
    /// Authenticated username / subject (the mandatory part of the identity).
    pub username: String,
    /// Optional email claim.
    pub email: Option<String>,
    /// Parsed group membership.
    pub groups: Vec<String>,
}

/// Split a groups header value into trimmed, non-empty parts using the fixed
/// `separator`. Whitespace around each entry is trimmed and empty entries dropped
/// so callers get a clean list regardless of proxy formatting.
pub fn parse_groups(value: &str, separator: &str) -> Vec<String> {
    value
        .split(separator)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether `remote` is among the trusted proxy source addresses.
pub fn source_is_trusted(remote: IpAddr, trusted: &[IpAddr]) -> bool {
    trusted.contains(&remote)
}

fn header_name(raw: &str) -> Option<HeaderName> {
    HeaderName::from_bytes(raw.as_bytes()).ok()
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let name = header_name(name)?;
    headers
        .get(&name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Extract a [`HeaderIdentity`] from `headers` when the configured user header
/// is present and non-empty. Returns `None` when the request carries no usable
/// identity headers, so callers can treat header auth as additive.
pub fn extract_header_identity(
    headers: &HeaderMap,
    cfg: &HeaderAuthConfig,
) -> Option<HeaderIdentity> {
    let username = header_value(headers, &cfg.user_header)?.trim().to_string();
    if username.is_empty() {
        return None;
    }
    let email = header_value(headers, &cfg.email_header)
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string());
    let groups = header_value(headers, &cfg.groups_header)
        .map(|v| parse_groups(&v, &cfg.groups_separator))
        .unwrap_or_default();
    Some(HeaderIdentity {
        username,
        email,
        groups,
    })
}

/// Remove any client-supplied copies of the identity headers from `request`.
///
/// This is the injection-safety guarantee: when the source is *not* trusted we
/// must not let a spoofed `Remote-User` (etc.) reach application logic. Only a
/// trusted proxy's headers survive, and the trusted branch reads them directly.
pub fn strip_identity_headers(request: &mut Request<Body>, cfg: &HeaderAuthConfig) {
    for name in [
        cfg.user_header.as_str(),
        cfg.email_header.as_str(),
        cfg.groups_header.as_str(),
    ] {
        if let Some(name) = header_name(name) {
            request.headers_mut().remove(name);
        }
    }
}

/// Test-only helper: set a header value, used by `api.rs` header-auth tests.
#[cfg(test)]
pub(crate) fn set_test_header(headers: &mut HeaderMap, name: &str, value: &str) {
    use axum::http::HeaderValue;
    if let (Some(name), Ok(v)) = (header_name(name), HeaderValue::from_str(value)) {
        headers.insert(name, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HeaderAuthConfig {
        HeaderAuthConfig::new()
    }

    #[test]
    fn groups_split_and_trim() {
        let cfg = cfg();
        assert_eq!(
            parse_groups("admin, user ,  root", &cfg.groups_separator),
            vec!["admin", "user", "root"]
        );
        assert_eq!(parse_groups("admin,,user", ","), vec!["admin", "user"]);
        assert_eq!(parse_groups("   ", ","), Vec::<String>::new());
        // Custom separator.
        assert_eq!(parse_groups("a|b| c", "|"), vec!["a", "b", "c"]);
    }

    #[test]
    fn trusted_source_membership() {
        let trusted: Vec<IpAddr> = vec!["127.0.0.1".parse().unwrap(), "10.0.0.1".parse().unwrap()];
        assert!(source_is_trusted("127.0.0.1".parse().unwrap(), &trusted));
        assert!(!source_is_trusted("192.168.1.5".parse().unwrap(), &trusted));
    }

    #[test]
    fn extract_requires_user_header() {
        let cfg = cfg();
        let mut headers = HeaderMap::new();
        assert!(extract_header_identity(&headers, &cfg).is_none());

        // Email only (no user header) -> no identity.
        set_test_header(&mut headers, "Remote-Email", "a@example.com");
        assert!(extract_header_identity(&headers, &cfg).is_none());

        // User header present -> identity with email + groups.
        set_test_header(&mut headers, "Remote-User", "alice");
        set_test_header(&mut headers, "Remote-Groups", "admin,user");
        let id = extract_header_identity(&headers, &cfg).unwrap();
        assert_eq!(id.username, "alice");
        assert_eq!(id.email.as_deref(), Some("a@example.com"));
        assert_eq!(id.groups, vec!["admin", "user"]);
    }

    #[test]
    fn extract_respects_custom_header_names() {
        let cfg = HeaderAuthConfig {
            user_header: "X-Forwarded-User".into(),
            email_header: "X-Forwarded-Email".into(),
            groups_header: "X-Forwarded-Groups".into(),
            groups_separator: ";".into(),
            ..HeaderAuthConfig::new()
        };
        let mut headers = HeaderMap::new();
        set_test_header(&mut headers, "X-Forwarded-User", "bob");
        set_test_header(&mut headers, "X-Forwarded-Groups", "g1; g2");
        let id = extract_header_identity(&headers, &cfg).unwrap();
        assert_eq!(id.username, "bob");
        assert_eq!(id.groups, vec!["g1", "g2"]);
    }

    #[test]
    fn strip_removes_identity_headers() {
        let cfg = cfg();
        let mut req = Request::builder().uri("/").body(Body::empty()).unwrap();
        set_test_header(req.headers_mut(), "Remote-User", "mallory");
        set_test_header(req.headers_mut(), "Remote-Email", "m@x.com");
        set_test_header(req.headers_mut(), "X-Other", "keep");
        strip_identity_headers(&mut req, &cfg);
        assert!(req.headers().get("Remote-User").is_none());
        assert!(req.headers().get("Remote-Email").is_none());
        assert!(req.headers().get("Remote-Groups").is_none());
        assert!(req.headers().get("X-Other").is_some());
    }
}
