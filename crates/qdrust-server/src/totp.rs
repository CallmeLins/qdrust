//! The server-side half of TOTP enrolment and verification.
//!
//! The arithmetic lives in [`qdrust_core::totp`], where template steps use it.
//! What is here is what only the login path needs: a fresh secret in the shape
//! an authenticator app accepts, the `otpauth://` URI to hand it over as, and a
//! verification window wide enough for a phone whose clock is a few seconds off.
//!
//! Nothing here generates a *code* the user could be told; enrolment completes
//! only when the user proves their own app produces the same code, which is why
//! `verify` is the only entry point the API uses.

use anyhow::{Result, bail};
use subtle::ConstantTimeEq;

/// Digits and step every authenticator app assumes when nothing else is said.
const DIGITS: u32 = 6;
const PERIOD: u64 = 30;
const ALGORITHM: &str = "sha1";

/// How many steps either side of `now` a code is accepted for.
///
/// One step each way is the RFC 6238 recommendation and covers a device clock
/// that is up to 30 seconds out in either direction. Widening it to two steps
/// would double the useful life of a stolen code, so it stays at one.
const SKEW_STEPS: i64 = 1;

/// A new 160-bit secret, base32-encoded with no padding -- the format RFC 4226
/// recommends for a shared key and the one every app accepts when typed by hand.
pub fn generate_secret() -> String {
    use rand::RngCore as _;
    let mut bytes = [0_u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    encode_base32(&bytes)
}

/// RFC 4648 base32 without padding (A-Z then 2-7).
fn encode_base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// The URI an authenticator app imports when the secret is shown as a QR code.
///
/// `issuer` should already be the display name the deployment uses; both the
/// label prefix and the `issuer` parameter carry it because apps disagree about
/// which one they read.
pub fn otpauth_uri(issuer: &str, account: &str, secret: &str) -> String {
    let issuer = if issuer.trim().is_empty() {
        "qdrust"
    } else {
        issuer.trim()
    };
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm={}&digits={}&period={}",
        encode_uri_component(issuer),
        encode_uri_component(account),
        secret,
        encode_uri_component(issuer),
        ALGORITHM.to_ascii_uppercase(),
        DIGITS,
        PERIOD
    )
}

/// Percent-encode the characters that cannot appear in a URI label.
///
/// `encode_uri_component` is small enough to write than to take a dependency
/// for: it only ever sees a username and an issuer name.
fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Whether `submitted` is the code for `secret` at `now`.
///
/// Compared in constant time: a login form that answers faster for a
/// nearly-right code tells an attacker how much of it was right.
pub fn verify(secret: &str, submitted: &str, now: u64) -> Result<bool> {
    let submitted: String = submitted
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect();
    if submitted.is_empty() {
        bail!("totp code is empty");
    }
    if submitted.len() != DIGITS as usize || !submitted.chars().all(|ch| ch.is_ascii_digit()) {
        bail!("totp code must be {DIGITS} digits");
    }
    for step in -SKEW_STEPS..=SKEW_STEPS {
        let offset = step * PERIOD as i64;
        let at = (now as i64 + offset).max(0) as u64;
        let expected = qdrust_core::totp::code(secret, DIGITS, PERIOD, ALGORITHM, at)?;
        if expected.as_bytes().ct_eq(submitted.as_bytes()).into() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Normalise a secret a user typed by hand: upper case, no spaces or padding.
///
/// Same tolerance as the verifier in core, applied here so the stored value is
/// already canonical and two enrolments of the same key compare equal.
pub fn normalize_secret(secret: &str) -> String {
    secret
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-' && *ch != '=')
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 random bytes must always encode to exactly 32 base32 characters, with
    /// no padding: that is what every app expects to import.
    #[test]
    fn generated_secrets_are_thirty_two_base32_characters() {
        for _ in 0..32 {
            let secret = generate_secret();
            assert_eq!(secret.len(), 32, "{secret}");
            assert!(
                secret
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ('2'..='7').contains(&ch)),
                "{secret}"
            );
        }
        // Two enrolments must not collide.
        assert_ne!(generate_secret(), generate_secret());
    }

    /// The encoder has to agree with the decoder the templates use, otherwise
    /// enrolling and verifying would disagree and every code would be rejected.
    #[test]
    fn encoding_round_trips_through_the_verifier() {
        let secret = generate_secret();
        let now = 1_700_000_000_u64;
        let expected = qdrust_core::totp::code(&secret, DIGITS, PERIOD, ALGORITHM, now).unwrap();
        assert!(verify(&secret, &expected, now).unwrap());
    }

    #[test]
    fn accepts_one_step_of_clock_skew_either_way() {
        let secret = generate_secret();
        let now = 1_700_000_000_u64;
        let previous =
            qdrust_core::totp::code(&secret, DIGITS, PERIOD, ALGORITHM, now - PERIOD).unwrap();
        let next =
            qdrust_core::totp::code(&secret, DIGITS, PERIOD, ALGORITHM, now + PERIOD).unwrap();
        assert!(verify(&secret, &previous, now).unwrap());
        assert!(verify(&secret, &next, now).unwrap());
        // But not two steps: that would keep a captured code alive twice as long.
        let stale =
            qdrust_core::totp::code(&secret, DIGITS, PERIOD, ALGORITHM, now - 2 * PERIOD).unwrap();
        assert!(!verify(&secret, &stale, now).unwrap());
    }

    #[test]
    fn rejects_malformed_input_instead_of_guessing() {
        let secret = generate_secret();
        let now = 1_700_000_000_u64;
        assert!(verify(&secret, "", now).is_err());
        assert!(verify(&secret, "12345", now).is_err());
        assert!(verify(&secret, "1234567", now).is_err());
        assert!(verify(&secret, "abcdef", now).is_err());
        // A well-formed but wrong code is a plain miss, not an error.
        assert!(!verify(&secret, "000000", now).unwrap());
    }

    /// A code typed with the grouping an app displays must still work.
    #[test]
    fn tolerates_the_spacing_people_type() {
        let secret = generate_secret();
        let now = 1_700_000_000_u64;
        let code = qdrust_core::totp::code(&secret, DIGITS, PERIOD, ALGORITHM, now).unwrap();
        let spaced = format!("{} {}", &code[..3], &code[3..]);
        assert!(verify(&secret, &spaced, now).unwrap());
    }

    #[test]
    fn builds_an_otpauth_uri_every_app_can_read() {
        let uri = otpauth_uri("qdrust", "alice", "ABCDEF234567");
        assert_eq!(
            uri,
            "otpauth://totp/qdrust:alice?secret=ABCDEF234567&issuer=qdrust&algorithm=SHA1&digits=6&period=30"
        );
        // A username with a space or an `@` must not break the label.
        let uri = otpauth_uri("qdrust", "a b@c", "ABCDEF234567");
        assert!(
            uri.starts_with("otpauth://totp/qdrust:a%20b%40c?secret="),
            "{uri}"
        );
        // An empty issuer still produces an importable URI.
        let uri = otpauth_uri("", "alice", "ABCDEF234567");
        assert!(uri.contains("issuer=qdrust"), "{uri}");
    }

    #[test]
    fn normalises_typed_secrets() {
        assert_eq!(normalize_secret("abcd efgh-2345===="), "ABCDEFGH2345");
    }
}
