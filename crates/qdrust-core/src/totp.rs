//! RFC 6238 time-based one-time passwords (TOTP).
//!
//! Kept in its own module because two callers need the same answer: the
//! `api://util/totp` built-in tool (a template step) and the `totp()` Jinja
//! function (a value inside a header or body). Both take the base32 secret the
//! user pasted from their 2FA setup page and nothing else leaves the process —
//! the point of computing it here is that the secret never goes to an external
//! API.
//!
//! The defaults match every authenticator app: 6 digits, a 30-second step and
//! HMAC-SHA1. `sha256` / `sha512` and other digit counts are accepted because
//! RFC 6238 defines them, not because sites use them.

use anyhow::{Result, bail};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Sha256, Sha512};

/// Compute the code for `secret` at unix time `at`.
///
/// `algorithm` is `sha1` (default), `sha256` or `sha512`; `digits` is 6..=10;
/// `period` is the step in seconds. The secret is base32, case-insensitive,
/// with `=` padding and spaces/hyphens tolerated (the shapes a setup page
/// shows).
pub fn code(secret: &str, digits: u32, period: u64, algorithm: &str, at: u64) -> Result<String> {
    if !(6..=10).contains(&digits) {
        bail!("totp digits must be between 6 and 10");
    }
    if period == 0 {
        bail!("totp period must be positive");
    }
    let key = decode_base32(secret)?;
    let counter = at / period;
    let digest = match algorithm.to_ascii_lowercase().as_str() {
        "sha1" => hmac_sha1(&key, &counter.to_be_bytes()),
        "sha256" => hmac_sha256(&key, &counter.to_be_bytes()),
        "sha512" => hmac_sha512(&key, &counter.to_be_bytes()),
        other => bail!("unsupported totp algorithm: {other}"),
    };
    // RFC 4226 dynamic truncation: the low nibble of the last byte picks a
    // 4-byte window, whose top bit is cleared before the modulo.
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let binary = ((digest[offset] as u32 & 0x7f) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | (digest[offset + 3] as u32);
    let code = binary % 10u32.pow(digits);
    Ok(format!("{code:0width$}", width = digits as usize))
}

// HMAC accepts keys of any length, so these cannot fail.
fn hmac_sha1(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn hmac_sha512(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha512> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// RFC 4648 base32, tolerant of the formatting a 2FA setup page shows.
fn decode_base32(secret: &str) -> Result<Vec<u8>> {
    let cleaned: String = secret
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect();
    let cleaned = cleaned.trim_end_matches('=').to_ascii_uppercase();
    if cleaned.is_empty() {
        bail!("totp secret is empty");
    }
    let mut out = Vec::with_capacity(cleaned.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for ch in cleaned.chars() {
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            '2'..='7' => ch as u32 - '2' as u32 + 26,
            other => bail!("invalid base32 character in totp secret: {other}"),
        };
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    if out.is_empty() {
        bail!("totp secret is too short");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RFC 6238 appendix B vectors, all with 8 digits and a 30-second step.
    #[test]
    fn matches_the_rfc_6238_vectors() {
        let cases: [(&str, &str, [&str; 6]); 3] = [
            (
                "sha1",
                "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
                [
                    "94287082", "07081804", "14050471", "89005924", "69279037", "65353130",
                ],
            ),
            (
                "sha256",
                "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA",
                [
                    "46119246", "68084774", "67062674", "91819424", "90698825", "77737706",
                ],
            ),
            (
                "sha512",
                "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA",
                [
                    "90693936", "25091201", "99943326", "93441116", "38618901", "47863826",
                ],
            ),
        ];
        let times = [
            59u64,
            1_111_111_109,
            1_111_111_111,
            1_234_567_890,
            2_000_000_000,
            20_000_000_000,
        ];
        for (algorithm, secret, expected) in cases {
            for (at, want) in times.iter().zip(expected) {
                assert_eq!(
                    code(secret, 8, 30, algorithm, *at).unwrap(),
                    want,
                    "{algorithm} at {at}"
                );
            }
        }
    }

    #[test]
    fn defaults_to_six_digits_and_accepts_the_setup_page_shapes() {
        // Lower case, spaces and padding are all how a setup page may show the
        // same secret; the code must not depend on the formatting.
        let canonical = code("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 6, 30, "sha1", 59).unwrap();
        assert_eq!(canonical, "287082");
        for variant in [
            "gezdgnbvgy3tqojqgezdgnbvgy3tqojq",
            "GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ",
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ====",
        ] {
            assert_eq!(code(variant, 6, 30, "sha1", 59).unwrap(), canonical);
        }
    }

    #[test]
    fn refuses_bad_parameters_instead_of_returning_a_wrong_code() {
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        assert!(code(secret, 5, 30, "sha1", 59).is_err());
        assert!(code(secret, 6, 0, "sha1", 59).is_err());
        assert!(code(secret, 6, 30, "md5", 59).is_err());
        assert!(code("not base32!", 6, 30, "sha1", 59).is_err());
        assert!(code("", 6, 30, "sha1", 59).is_err());
    }
}
