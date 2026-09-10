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
    AccessToken, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet, EndpointSet,
    IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, SubjectIdentifier,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata, CoreUserInfoClaims},
};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::config::OidcConfig;

/// A [`CoreClient`] built from OIDC discovery metadata: the authorization
/// endpoint is always present (`EndpointSet`), the token and user-info
/// endpoints may or may not be advertised (`EndpointMaybeSet`), and the
/// device/introspection/revocation endpoints are not used. This concrete type is
/// what `CoreClient::from_provider_metadata` + `set_redirect_uri` yields, so the
/// authorization-code flow can call the fallible `exchange_code`.
pub type OidcClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

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

/// Decode the (unverified) JSON payload of a compact JWT. The token must have
/// already passed signature verification upstream (`IdToken::claims`) before
/// any caller trusts what this returns; we only read profile fields for display.
fn decode_id_token_payload(id_token: &str) -> Option<serde_json::Value> {
    let payload_b64 = match id_token.split('.').collect::<Vec<_>>().as_slice() {
        [_, payload, _] => *payload,
        _ => return None,
    };
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    serde_json::from_slice(&payload_bytes).ok()
}

/// TEMP DEBUG — decode an already-verified ID token's payload for logging, so a
/// tester can capture exactly which claims an IdP emitted (preferred_username /
/// nickname / name / email / sub) and confirm the username-picking root cause.
/// Call ONLY after `id_token.claims()` has verified the signature upstream.
/// Remove once the diagnosis is settled.
pub fn debug_decode_claims(id_token: &str) -> Option<serde_json::Value> {
    decode_id_token_payload(id_token)
}

/// Extract a group-membership claim from an already-verified compact ID token.
///
/// The openidconnect crate binds the parsed ID token to `EmptyAdditionalClaims`,
/// so non-standard claims such as `groups` are dropped at deserialization time.
/// Because the token has already passed signature/nonce verification before this
/// is called (`IdToken::claims` succeeds first), re-decoding its base64url payload
/// here is safe: we are only reading group membership off a JWT whose integrity
/// was just confirmed, never trusting an unverified payload.
///
/// Supports the common shapes IdPs emit for the claim:
///   * a JSON array of strings: `{"groups": ["qdrust-admins", "users"]}`
///   * a single string:         `{"groups": "qdrust-admins,users"}`
///
/// Anything else (absent claim, not an array of strings, non-string) yields an
/// empty vec so the caller falls back to `default_role`.
pub fn groups_from_id_token(id_token: &str, claim: &str) -> Vec<String> {
    let Some(payload) = decode_id_token_payload(id_token) else {
        return Vec::new();
    };
    match payload.get(claim) {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(serde_json::Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// True when `s` looks like an opaque identifier assigned by an IdP rather
/// than a human-chosen handle: a long all-hex string (lower/upper case),
/// a canonical UUID, or a long run of digits. Such values are fine as a `sub`
/// but make a poor local login name (they render as a long "hex id" in the UI),
/// so the username-picking logic below skips them in favour of a readable
/// `name`/`email` when one exists.
pub fn looks_like_opaque_id(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    let all_hex = |x: &str| x.chars().all(|c| c.is_ascii_hexdigit());
    // Long hex blob (>= 16 chars), e.g. "a3f9c1..." or a hex sha/uuid-without-dashes.
    let long_hex = t.len() >= 16 && all_hex(t);
    // Canonical UUID form: 8-4-4-4-12 of hex.
    let uuid_form = {
        let parts: Vec<&str> = t.split('-').collect();
        parts.len() == 5 && parts.iter().all(|p| !p.is_empty() && all_hex(p)) && t.len() == 36
    };
    // Long numeric id (>= 8 digits), e.g. GitHub-style numeric ids.
    let long_digits = t.len() >= 8 && t.chars().all(|c| c.is_ascii_digit());
    long_hex || uuid_form || long_digits
}

/// Pick a human-friendly local username hint from an already-verified ID token.
///
/// Many IdPs only put `name`/`email` in the ID token and omit (or make opaque)
/// `preferred_username`, in which case the generic subject (`sub`) — often a
/// long random id — would otherwise become the user's login name. Some IdPs go
/// further and ship an *opaque* `preferred_username` (a hex/uuid/numeric handle)
/// while the real, readable handle only lives in `name`/`email`; blindly
/// trusting `preferred_username` first would then persist that opaque string.
///
/// To handle both, we consider candidates in order `preferred_username` →
/// `nickname` → `name` → email local-part but **skip any candidate that looks
/// like an opaque id** (see [`looks_like_opaque_id`]) so a readable `name`/
/// `email` still wins. Only if every candidate is opaque/absent do we return
/// `None`, letting the caller keep the `sub`.
///
/// `name` may be an object `{ "value": ... }` for localized claims, so read both
/// a bare string and a `value` field. Callers sanitize the result.
///
/// `typed_preferred_username` is the value already decoded through the
/// openidconnect crate's typed claim accessor (kept separate because that crate
/// may drop non-standard or localized forms); it is merged in at the front of
/// the chain.
pub fn username_hint_from_id_token(
    typed_preferred_username: Option<&str>,
    id_token: &str,
) -> Option<String> {
    let payload = decode_id_token_payload(id_token);
    let pick = |key: &str| -> Option<String> {
        let payload = payload.as_ref()?;
        match payload.get(key) {
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                Some(s.trim().to_string())
            }
            Some(serde_json::Value::Object(map)) => map
                .get("value")
                .and_then(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string()),
            _ => None,
        }
    };
    // The typed accessor is merged ahead of the raw `preferred_username` so a
    // localized/typed form is still considered first; `username_hint_from_
    // candidates` dedupes the two identical values.
    let raw_preferred = pick("preferred_username");
    let nickname = pick("nickname");
    let name = pick("name");
    let email = pick("email");
    username_hint_from_candidates(
        typed_preferred_username.or(raw_preferred.as_deref()),
        nickname.as_deref(),
        name.as_deref(),
        email.as_deref(),
    )
}

/// The human-readable profile fields we read back from a provider's UserInfo
/// endpoint (or, as a fallback, from the ID token). Only the fields needed to
/// pick a local username and match an existing account are kept; `sub` is
/// carried separately by the caller.
#[derive(Clone, Debug, Default)]
pub struct UserInfoProfile {
    pub preferred_username: Option<String>,
    pub nickname: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
}

impl UserInfoProfile {
    /// True when the profile carries no usable identity field at all.
    pub fn is_empty(&self) -> bool {
        self.preferred_username.is_none()
            && self.nickname.is_none()
            && self.name.is_none()
            && self.email.is_none()
    }
}

/// Call the provider's OIDC UserInfo endpoint with the freshly-issued access
/// token and return the profile claims it carries.
///
/// Many IdPs (authentik with a minimal scope mapper, Keycloak with only the
/// default ID-token claims, and various enterprise providers) ship an ID token
/// that contains **only** `sub` and put `preferred_username` / `name` / `email`
/// behind the UserInfo endpoint instead. Without this call the local username
/// would fall back to the opaque `sub` and show up as a long id in the UI.
///
/// Returns `Ok(None)` when the provider advertises no userinfo endpoint (nothing
/// to fetch) or when the call fails — a UserInfo problem must never block a
/// login that already verified the ID token, so failures degrade to "no extra
/// profile" and the caller falls back to the ID-token claims / `sub`.
///
/// `expected_subject` is the verified `sub` from the ID token; passing it makes
/// the crate reject a UserInfo response for a different subject (token
/// substitution defence).
pub async fn fetch_user_info(
    client: &OidcClient,
    http: &reqwest::Client,
    access_token: &str,
    expected_subject: &str,
) -> Option<UserInfoProfile> {
    let subject = SubjectIdentifier::new(expected_subject.to_string());
    let request = match client.user_info(AccessToken::new(access_token.to_string()), Some(subject))
    {
        Ok(req) => req,
        Err(e) => {
            // Most commonly: the provider advertises no userinfo endpoint.
            tracing::debug!(error = %e, "oidc userinfo unavailable; skipping");
            return None;
        }
    };
    let claims: CoreUserInfoClaims = match request.request_async(http).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "oidc userinfo request failed; continuing without it");
            return None;
        }
    };
    Some(UserInfoProfile {
        preferred_username: claims
            .preferred_username()
            .map(|u| u.as_str().trim().to_string())
            .filter(|s| !s.is_empty()),
        // `name`/`nickname` are localized claims (`Option<&LocalizedClaim<_>>`);
        // `get(None)` reads the un-tagged/default value.
        nickname: claims
            .nickname()
            .and_then(|c| c.get(None))
            .map(|n| n.as_str().trim().to_string())
            .filter(|s| !s.is_empty()),
        name: claims
            .name()
            .and_then(|c| c.get(None))
            .map(|n| n.as_str().trim().to_string())
            .filter(|s| !s.is_empty()),
        email: claims
            .email()
            .map(|e| e.as_str().trim().to_string())
            .filter(|s| !s.is_empty()),
    })
}

/// Pick a human-friendly local username from a merged set of candidate claims.
///
/// This is the shared selection core used for both the ID-token claims and the
/// UserInfo profile. Candidates are considered in order `preferred_username` →
/// `nickname` → `name` → email local-part, **skipping any candidate that looks
/// like an opaque id** (see [`looks_like_opaque_id`]) so a readable `name`/
/// `email` still wins over an IdP-assigned hex/uuid handle. Returns `None` when
/// every candidate is opaque or absent, letting the caller fall back to `sub`.
pub fn username_hint_from_candidates(
    preferred_username: Option<&str>,
    nickname: Option<&str>,
    name: Option<&str>,
    email: Option<&str>,
) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    let mut prefer = |v: &str| -> Option<String> {
        let v = v.trim();
        if v.is_empty() || looks_like_opaque_id(v) || !seen.insert(v.to_string()) {
            return None;
        }
        Some(v.to_string())
    };
    for candidate in [preferred_username, nickname, name].into_iter().flatten() {
        if let Some(v) = prefer(candidate) {
            return Some(v);
        }
    }
    // Email local part is a reasonable default handle when nothing else is set.
    if let Some(email) = email
        && let Some(local) = email.split('@').next()
        && let Some(v) = prefer(local)
    {
        return Some(v);
    }
    None
}

/// Derive a short, readable login handle from an opaque subject identifier.
///
/// This is the last-resort fallback for IdPs whose ID token carries **only**
/// `sub` (a long hex/uuid/numeric id) and whose UserInfo endpoint either is not
/// advertised or also returns no readable profile. Persisting the raw `sub` makes
/// the account show up as a 36-char uuid in the UI, so we shorten it to a stable,
/// human-scannable handle instead.
///
/// The result is `user-<first 8 hex-ish chars>` (e.g. `user-23cc07f8`), with any
/// non-alphanumeric separators (uuid dashes, dots, etc.) stripped before taking
/// the prefix so the handle stays `[a-z0-9-]`-clean. A blank subject yields the
/// bare `user` prefix. The mapping is deterministic, so the same `sub` always
/// produces the same handle and re-logins stay stable.
pub fn username_fallback_from_subject(subject: &str) -> String {
    const PREFIX: &str = "user";
    const TAIL: usize = 8;
    let cleaned: String = subject
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if cleaned.is_empty() {
        return PREFIX.to_string();
    }
    let tail: String = cleaned.chars().take(TAIL).collect();
    format!("{PREFIX}-{tail}")
}

/// Compute the redirect_uri handed to the IdP.
///
/// Prefers `X-Forwarded-Proto` / `X-Forwarded-Host` (trusted reverse-proxy
/// scenario) and falls back to the request's own scheme/host. The path is
/// `{base_path}/api/v1/auth/oidc/callback`, so the same value is recomputed at
/// callback time and must match what the IdP was configured with.
///
/// Note: for an origin-form HTTP request with no `X-Forwarded-*` headers the
/// request's own scheme/authority are unavailable (the HTTP/1.1 request line
/// carries only a path), so this falls back to `https://localhost/...`. Direct
/// HTTP deployments (no reverse proxy) should set the explicit
/// `QDRUST_OIDC_REDIRECT_URI` override instead (see [`effective_redirect_uri`]).
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

/// Resolve the redirect_uri a given request should use.
///
/// If the operator configured an explicit `redirect_uri` (`QDRUST_OIDC_
/// REDIRECT_URI`) it is authoritative and returned verbatim. Otherwise the
/// value is derived at request time from the request host + base path (see
/// [`derive_redirect_uri`]) so a single image serves any sub-path behind a
/// trusted reverse proxy.
///
/// Both the start and callback handlers must call this so they agree on the
/// same redirect_uri (a mismatch would fail the callback's `redirect_uri`
/// check).
pub fn effective_redirect_uri(
    oidc: &OidcConfig,
    base_path: &str,
    headers: &HeaderMap,
    uri: &Uri,
) -> String {
    let configured = oidc.redirect_uri.trim();
    if !configured.is_empty() {
        configured.to_string()
    } else {
        derive_redirect_uri(base_path, headers, uri)
    }
}

/// Result of [`build_client`]: the discovery-configured [`OidcClient`] plus the
/// stateful `reqwest::Client` used to reach the provider. The HTTP client must be
/// retained because openidconnect 4.x `request_async` takes an `AsyncHttpClient`
/// by reference (it cannot be reconstructed from just the `OidcClient`).
pub struct BuiltClient {
    pub client: OidcClient,
    pub http: reqwest::Client,
}

/// Discover the provider metadata and build a [`OidcClient`] configured with the
/// redirect_uri, returning the shared stateful HTTP client alongside it.
///
/// Redirect-following is disabled on the HTTP client (openidconnect 4.x SSRF
/// guidance); discovery + JWKS fetch + the later token exchange all reuse it.
pub async fn build_client(oidc: &OidcConfig, redirect_uri: &str) -> anyhow::Result<BuiltClient> {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build OIDC http client: {e}"))?;
    let issuer_url = IssuerUrl::new(oidc.issuer.clone())
        .map_err(|e| anyhow::anyhow!("invalid OIDC issuer URL: {e}"))?;
    let metadata = CoreProviderMetadata::discover_async(issuer_url, &http)
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
    Ok(BuiltClient { client, http })
}

/// Build the provider authorization URL, installing our deterministic
/// `state`/`nonce` and the PKCE S256 challenge derived from `verifier`.
pub fn build_authorize_url(
    client: &OidcClient,
    raw_state: &str,
    nonce: &str,
    verifier: &str,
    scopes: &[Scope],
) -> String {
    let verifier_obj = PkceCodeVerifier::new(verifier.to_string());
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&verifier_obj);
    let raw_state_owned = raw_state.to_string();
    let nonce_owned = nonce.to_string();
    // The openidconnect crate always adds the `openid` scope itself (the client
    // is built with `use_openid_scope`). Passing it here too would emit a
    // duplicate `scope=openid openid ...`; strip it and dedupe the rest so the
    // authorization URL carries each scope exactly once.
    let mut seen: Vec<String> = Vec::new();
    let extra: Vec<Scope> = scopes
        .iter()
        .filter(|s| {
            let name = s.to_string();
            if name == "openid" || seen.contains(&name) {
                return false;
            }
            seen.push(name);
            true
        })
        .cloned()
        .collect();
    let (authorize_url, _, _) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            move || CsrfToken::new(raw_state_owned.clone()),
            move || Nonce::new(nonce_owned.clone()),
        )
        .add_scopes(extra)
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
    fn effective_redirect_uri_prefers_configured_override() {
        let headers = HeaderMap::new();
        let uri = Uri::from_static("/api/v1/auth/oidc/start");
        // Configured override wins verbatim over the runtime derivation.
        let oidc = OidcConfig {
            redirect_uri: "https://sso.example.com/custom/callback".into(),
            ..OidcConfig::default()
        };
        assert_eq!(
            effective_redirect_uri(&oidc, "", &headers, &uri),
            "https://sso.example.com/custom/callback"
        );
    }

    #[test]
    fn effective_redirect_uri_derives_when_no_override() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("auth.example.com"),
        );
        let uri = Uri::from_static("/qd/api/v1/auth/oidc/start");
        let oidc = OidcConfig::default(); // redirect_uri empty
        assert_eq!(
            effective_redirect_uri(&oidc, "/qd", &headers, &uri),
            "https://auth.example.com/qd/api/v1/auth/oidc/callback"
        );
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

    /// Build a compact JWT string whose payload is `payload_json`, for testing
    /// the payload-only extraction helper. Signature/header are dummy bytes.
    fn fake_id_token(payload_json: serde_json::Value) -> String {
        let payload = URL_SAFE_NO_PAD.encode(payload_json.to_string().as_bytes());
        let header = URL_SAFE_NO_PAD.encode(b"{\"alg\":\"RS256\"}");
        let sig = URL_SAFE_NO_PAD.encode(b"sig");
        format!("{header}.{payload}.{sig}")
    }

    #[test]
    fn groups_extracted_from_array_claim() {
        let token = fake_id_token(serde_json::json!({
            "sub": "abc",
            "groups": ["qdrust-admins", " users ", "qdrust-dev"]
        }));
        let groups = groups_from_id_token(&token, "groups");
        assert_eq!(groups, vec!["qdrust-admins", "users", "qdrust-dev"]);
    }

    #[test]
    fn groups_extracted_from_comma_string_claim() {
        let token = fake_id_token(serde_json::json!({
            "groups": "qdrust-admins, qdrust-dev"
        }));
        assert_eq!(
            groups_from_id_token(&token, "groups"),
            vec!["qdrust-admins", "qdrust-dev"]
        );
    }

    #[test]
    fn missing_claim_or_bad_payload_yields_empty() {
        // No groups claim at all -> empty (falls back to default_role).
        let token = fake_id_token(serde_json::json!({ "sub": "abc" }));
        assert!(groups_from_id_token(&token, "groups").is_empty());
        // Wrong claim name -> empty.
        assert!(groups_from_id_token(&token, "roles").is_empty());
        // Malformed JWT / non-object -> empty.
        assert!(groups_from_id_token("not-a-jwt", "groups").is_empty());
        // Groups present but not strings -> empty.
        let bad = fake_id_token(serde_json::json!({ "groups": 42 }));
        assert!(groups_from_id_token(&bad, "groups").is_empty());
    }

    #[test]
    fn username_hint_prefers_preferred_username_then_nickname_then_name() {
        let token = fake_id_token(serde_json::json!({
            "sub": "11111111-2222-3333-4444-555555555555",
            "preferred_username": "alice",
            "nickname": "ali",
            "name": "Alice Example",
            "email": "alice@example.com",
        }));
        assert_eq!(
            username_hint_from_id_token(None, &token).as_deref(),
            Some("alice")
        );
        // No preferred_username -> nickname.
        let token = fake_id_token(serde_json::json!({
            "sub": "11111111-2222-3333-4444-555555555555",
            "nickname": "ali",
            "name": "Alice Example",
        }));
        assert_eq!(
            username_hint_from_id_token(None, &token).as_deref(),
            Some("ali")
        );
        // Only name -> name.
        let token = fake_id_token(serde_json::json!({
            "sub": "11111111-2222-3333-4444-555555555555",
            "name": "Alice Example",
        }));
        assert_eq!(
            username_hint_from_id_token(None, &token).as_deref(),
            Some("Alice Example")
        );
    }

    #[test]
    fn username_hint_handles_localized_name_and_email_fallback() {
        // Localized `name` is an object with a `value` field.
        let token = fake_id_token(serde_json::json!({
            "sub": "11111111-2222-3333-4444-555555555555",
            "name": { "value": "Bob Builder" },
        }));
        assert_eq!(
            username_hint_from_id_token(None, &token).as_deref(),
            Some("Bob Builder")
        );
        // Only an email -> use its local part.
        let token = fake_id_token(serde_json::json!({
            "sub": "11111111-2222-3333-4444-555555555555",
            "email": "carol@example.com",
        }));
        assert_eq!(
            username_hint_from_id_token(None, &token).as_deref(),
            Some("carol")
        );
        // Nothing useful -> None so the caller keeps the opaque sub.
        let token =
            fake_id_token(serde_json::json!({ "sub": "11111111-2222-3333-4444-555555555555" }));
        assert!(username_hint_from_id_token(None, &token).is_none());
        assert!(username_hint_from_id_token(None, "not-a-jwt").is_none());
    }

    #[test]
    fn opaque_preferred_username_skipped_for_readable_name_or_email() {
        // An IdP that ships an opaque preferred_username but a readable name:
        // the opaque handle must NOT win; name is preferred.
        let token = fake_id_token(serde_json::json!({
            "sub": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "preferred_username": "3f9c1e2a7b4d8f0a6c3e9d1b5a7f0c2e",
            "nickname": "christa",
            "name": "christaikobo",
            "email": "christaikobo@example.com",
        }));
        assert_eq!(
            username_hint_from_id_token(None, &token).as_deref(),
            Some("christa")
        );

        // Opaque preferred_username + only an email local-part readable.
        let token = fake_id_token(serde_json::json!({
            "sub": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "preferred_username": "3f9c1e2a7b4d8f0a6c3e9d1b5a7f0c2e",
            "email": "christaikobo@example.com",
        }));
        assert_eq!(
            username_hint_from_id_token(None, &token).as_deref(),
            Some("christaikobo")
        );

        // The typed preferred_username (passed separately, as the callback does)
        // is opaque too -> it must not win over the readable email local-part.
        let typed = "3f9c1e2a7b4d8f0a6c3e9d1b5a7f0c2e";
        assert_eq!(
            username_hint_from_id_token(Some(typed), &token).as_deref(),
            Some("christaikobo")
        );

        // Everything opaque (preferred_username hex + sub uuid + no name/email)
        // -> None so the caller keeps the sub.
        let token =
            fake_id_token(serde_json::json!({ "sub": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee" }));
        assert!(username_hint_from_id_token(Some(typed), &token).is_none());
    }

    #[test]
    fn opaque_id_detection_heuristic() {
        assert!(looks_like_opaque_id("3f9c1e2a7b4d8f0a6c3e9d1b5a7f0c2e")); // long hex
        assert!(looks_like_opaque_id("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")); // uuid
        assert!(looks_like_opaque_id("12345678901234")); // long digits
        assert!(!looks_like_opaque_id("christaikobo")); // human
        assert!(!looks_like_opaque_id("alice")); // short, not hex/digits
        assert!(looks_like_opaque_id("")); // blank is not a usable handle either
    }

    #[test]
    fn candidates_pick_readable_handle_and_skip_opaque() {
        // Preferred username wins when readable.
        assert_eq!(
            username_hint_from_candidates(
                Some("christaikobo"),
                Some("christa"),
                Some("Christa Iko"),
                Some("christaikobo@example.com"),
            )
            .as_deref(),
            Some("christaikobo")
        );
        // Opaque preferred_username is skipped in favour of nickname.
        assert_eq!(
            username_hint_from_candidates(
                Some("3f9c1e2a7b4d8f0a6c3e9d1b5a7f0c2e"),
                Some("christa"),
                None,
                None,
            )
            .as_deref(),
            Some("christa")
        );
        // Everything opaque/absent -> None (caller keeps the sub).
        assert!(
            username_hint_from_candidates(
                Some("3f9c1e2a7b4d8f0a6c3e9d1b5a7f0c2e"),
                None,
                None,
                None,
            )
            .is_none()
        );
        // No candidates at all -> None.
        assert!(username_hint_from_candidates(None, None, None, None).is_none());
    }

    #[test]
    fn candidates_use_email_local_part_as_last_resort() {
        // This mirrors the reported real-world case: an IdP whose ID token has
        // only `sub`, with the readable handle reachable only via UserInfo's
        // `email` field.
        assert_eq!(
            username_hint_from_candidates(None, None, None, Some("christaikobo@example.com"))
                .as_deref(),
            Some("christaikobo")
        );
        // Opaque email local part is rejected too.
        assert!(
            username_hint_from_candidates(None, None, None, Some("1234567890@x.com")).is_none()
        );
    }

    #[test]
    fn user_info_profile_is_empty_detects_blank_profiles() {
        assert!(UserInfoProfile::default().is_empty());
        let with_email = UserInfoProfile {
            email: Some("a@b.com".into()),
            ..UserInfoProfile::default()
        };
        assert!(!with_email.is_empty());
    }

    #[test]
    fn subject_fallback_shortens_opaque_sub_to_readable_handle() {
        // The reported real-world case: an ID token whose only claim is a uuid
        // `sub`. The fallback must not surface the raw uuid.
        assert_eq!(
            username_fallback_from_subject("23cc07f8-redacted-b40581b80dcb"),
            "user-23cc07f8"
        );
        // Canonical uuid -> first 8 hex chars, dashes stripped.
        assert_eq!(
            username_fallback_from_subject("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            "user-aaaaaaaa"
        );
        // Long hex and long numeric ids work the same way.
        assert_eq!(
            username_fallback_from_subject("3f9c1e2a7b4d8f0a6c3e9d1b5a7f0c2e"),
            "user-3f9c1e2a"
        );
        assert_eq!(
            username_fallback_from_subject("12345678901234"),
            "user-12345678"
        );
        // Short/blank subjects degrade gracefully instead of panicking.
        assert_eq!(username_fallback_from_subject("ab"), "user-ab");
        assert_eq!(username_fallback_from_subject(""), "user");
        assert_eq!(username_fallback_from_subject("   "), "user");
        // Deterministic: same sub -> same handle across logins.
        assert_eq!(
            username_fallback_from_subject("23cc07f8-redacted-b40581b80dcb"),
            username_fallback_from_subject("23cc07f8-redacted-b40581b80dcb")
        );
        // The derived handle must itself look like a readable (non-opaque) name.
        assert!(!looks_like_opaque_id(&username_fallback_from_subject(
            "23cc07f8-redacted-b40581b80dcb"
        )));
    }
}
