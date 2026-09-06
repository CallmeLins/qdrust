//! OpenID Connect Authorization Code + PKCE helpers for qdrust (Phase 1).
//!
//! This module contains the pure logic for the OIDC flow. It deliberately avoids
//! depending on axum *handlers*; the axum entry points live in `api.rs` and call
//! into these functions. The only axum types used here are the plain
//! [`axum::http`] primitives (`HeaderMap` / `Uri`), which are just HTTP types.
//!
//! Security design (see implementation brief):
//! * The PKCE code verifier and the OIDC nonce are derived *deterministically*
//!   from a high-entropy random `state` value plus the (server-only) client
//!   secret: `verifier = b64url(sha256(state || client_secret))` and
//!   `nonce = b64url(sha256("nonce" || state || client_secret))`.
//!   The DB row therefore only needs to store hashes for integrity; the real
//!   verifier/nonce are recomputed at callback time and never persist in
//!   recoverable form. This also makes the flow restart-safe and multi-instance
//!   safe with a shared DB.

use axum::http::{HeaderMap, Uri};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openidconnect::{
    ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
    reqwest::async_http_client,
};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::config::OidcConfig;

/// Cookie carrying the raw `state` back from the IdP on the top-level redirect.
/// Must be `SameSite=Lax` so it is sent on the cross-site navigation back from
/// the identity provider.
pub const OIDC_STATE_COOKIE: &str = "qd_oidc_state";
/// The OIDC callback mount point (suffix appended to the configured base path).
pub const OIDC_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";
/// How long a single-use login state row stays valid (seconds).
pub const OIDC_STATE_TTL_SECS: i64 = 300;

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn random_url_safe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// Generate a fresh high-entropy raw `state` value (the only secret the DB row
/// is keyed on). Returns a URL-safe base64 string.
pub fn generate_state() -> String {
    random_url_safe(32)
}

/// Derive the PKCE code verifier from the raw `state` and the client secret.
/// Deterministic and server-only: never stored in recoverable form.
pub fn derive_pkce_verifier(state: &str, client_secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(state.as_bytes());
    hasher.update(client_secret.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Derive the OIDC nonce from the raw `state` and the client secret.
pub fn derive_nonce(state: &str, client_secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nonce");
    hasher.update(state.as_bytes());
    hasher.update(client_secret.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// sha256 hash of the raw `state`, used as the DB row key.
pub fn state_hash(state: &str) -> String {
    sha256_hex(state.as_bytes())
}

/// sha256 hash of the derived nonce, stored for integrity verification.
pub fn nonce_hash(nonce: &str) -> String {
    sha256_hex(nonce.as_bytes())
}

/// sha256 hash of the derived verifier, stored for integrity verification.
pub fn verifier_hash(verifier: &str) -> String {
    sha256_hex(verifier.as_bytes())
}

/// Parse the configured scope string (space/comma/semicolon separated) into
/// [`Scope`] values. Defaults already handle "openid profile email"; we still
/// forward whatever the operator configured.
pub fn parse_scopes(scopes: &str) -> Vec<Scope> {
    scopes
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|s| !s.is_empty())
        .map(|s| Scope::new(s.to_string()))
        .collect()
}

/// Compute the redirect_uri handed to the IdP.
///
/// Prefers `X-Forwarded-Proto` / `X-Forwarded-Host` (trusted reverse-proxy
/// scenario) and falls back to the request's own scheme/host. The path is
/// `{base_path}/api/v1/auth/oidc/callback`, so the same value is recomputed at
/// callback time and must match what the IdP was configured with.
pub fn derive_redirect_uri(base_path: &str, headers: &HeaderMap, uri: &Uri) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uri.scheme_str().unwrap_or("https"));
    let host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .or_else(|| uri.authority().map(|a| a.as_str()))
        .unwrap_or("localhost");
    let base = base_path.trim_end_matches('/');
    format!("{scheme}://{host}{base}{OIDC_CALLBACK_PATH}")
}

/// Discover the provider metadata and build a [`CoreClient`] configured with the
/// redirect_uri. Uses the async reqwest backend so it can run inside the tokio
/// runtime without blocking a worker thread.
pub async fn build_client(oidc: &OidcConfig, redirect_uri: &str) -> anyhow::Result<CoreClient> {
    let issuer_url = IssuerUrl::new(oidc.issuer.clone())
        .map_err(|e| anyhow::anyhow!("invalid OIDC issuer URL: {e}"))?;
    let metadata = CoreProviderMetadata::discover_async(issuer_url, async_http_client)
        .await
        .map_err(|e| anyhow::anyhow!("OIDC discovery failed: {e}"))?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(oidc.client_id.clone()),
        Some(ClientSecret::new(oidc.client_secret.clone())),
    )
    .set_redirect_uri(
        RedirectUrl::new(redirect_uri.to_string())
            .map_err(|e| anyhow::anyhow!("invalid OIDC redirect URI: {e}"))?,
    );
    Ok(client)
}

/// Build the provider authorization URL, installing our deterministic
/// `state`/`nonce` and the PKCE S256 challenge derived from `verifier`.
pub fn build_authorize_url(
    client: &CoreClient,
    raw_state: &str,
    nonce: &str,
    verifier: &str,
    scopes: &[Scope],
) -> String {
    let verifier_obj = PkceCodeVerifier::new(verifier.to_string());
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&verifier_obj);
    let raw_state_owned = raw_state.to_string();
    let nonce_owned = nonce.to_string();
    let (authorize_url, _, _) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            move || CsrfToken::new(raw_state_owned.clone()),
            move || Nonce::new(nonce_owned.clone()),
        )
        .add_scopes(scopes.to_vec())
        .set_pkce_challenge(challenge)
        .url();
    authorize_url.to_string()
}

/// Append a `Set-Cookie` header that expires (clears) the OIDC state cookie.
pub fn append_clear_oidc_cookie(headers: &mut HeaderMap, secure: bool) {
    let secure_suffix = if secure { "; Secure" } else { "" };
    let cookie =
        format!("{OIDC_STATE_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure_suffix}");
    if let Ok(value) = axum::http::HeaderValue::from_str(&cookie) {
        headers.append(axum::http::header::SET_COOKIE, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderValue, Uri};

    #[test]
    fn verifier_and_nonce_are_deterministic_and_distinct() {
        let v1 = derive_pkce_verifier("state", "secret");
        let v2 = derive_pkce_verifier("state", "secret");
        assert_eq!(v1, v2, "verifier must be deterministic");
        let n = derive_nonce("state", "secret");
        assert_eq!(
            n,
            derive_nonce("state", "secret"),
            "nonce must be deterministic"
        );
        assert_ne!(v1, n, "verifier and nonce must differ");
        // PKCE verifier must be 43-128 unreserved chars; b64url(sha256) = 43.
        assert_eq!(v1.len(), 43);
        // Different state -> different verifier.
        assert_ne!(derive_pkce_verifier("other", "secret"), v1);
    }

    #[test]
    fn hashes_are_stable() {
        assert_eq!(state_hash("abc"), state_hash("abc"));
        assert_eq!(nonce_hash("xyz"), nonce_hash("xyz"));
        assert_eq!(verifier_hash("123"), verifier_hash("123"));
        assert_ne!(state_hash("abc"), state_hash("abd"));
    }

    #[test]
    fn redirect_uri_uses_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("auth.example.com"),
        );
        let uri = Uri::from_static("http://internal:8080/qd/api/v1/auth/oidc/start");
        let got = derive_redirect_uri("/qd", &headers, &uri);
        assert_eq!(got, "https://auth.example.com/qd/api/v1/auth/oidc/callback");
    }

    #[test]
    fn redirect_uri_falls_back_to_request_uri() {
        let headers = HeaderMap::new();
        let uri = Uri::from_static("http://internal:8080/api/v1/auth/oidc/start");
        let got = derive_redirect_uri("", &headers, &uri);
        assert_eq!(got, "http://internal:8080/api/v1/auth/oidc/callback");
    }

    #[test]
    fn parse_scopes_splits_on_whitespace_and_commas() {
        let scopes = parse_scopes("openid profile email");
        assert_eq!(scopes.len(), 3);
        assert_eq!(scopes[0], Scope::new("openid".to_string()));
        let scoped = parse_scopes("openid, profile ,email;groups");
        assert_eq!(scoped.len(), 4);
    }

    #[test]
    fn generate_state_is_high_entropy_and_url_safe() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }
}
