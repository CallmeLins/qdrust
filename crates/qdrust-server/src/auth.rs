use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand::RngCore;
use sha2::{Digest, Sha256};

const TOKEN_BYTES: usize = 32;
const SALT_BYTES: usize = 16;

#[derive(Clone, Default)]
pub struct LoginRateLimiter {
    state: Arc<tokio::sync::Mutex<HashMap<String, AttemptWindow>>>,
    max_attempts: u32,
    window: Duration,
}

#[derive(Clone, Copy)]
struct AttemptWindow {
    started: Instant,
    failures: u32,
}

impl LoginRateLimiter {
    pub fn new(max_attempts: u32, window: Duration) -> Result<Self> {
        ensure!(
            max_attempts > 0,
            "login rate limit must allow at least one attempt"
        );
        ensure!(!window.is_zero(), "login rate limit window cannot be zero");
        Ok(Self {
            state: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            max_attempts,
            window,
        })
    }

    pub async fn allowed(&self, key: &str) -> bool {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        let Some(window) = state.get(key).copied() else {
            return true;
        };
        if now.duration_since(window.started) >= self.window {
            state.remove(key);
            return true;
        }
        window.failures < self.max_attempts
    }

    pub async fn record_failure(&self, key: &str) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        let window = state.entry(key.to_owned()).or_insert(AttemptWindow {
            started: now,
            failures: 0,
        });
        if now.duration_since(window.started) >= self.window {
            *window = AttemptWindow {
                started: now,
                failures: 0,
            };
        }
        window.failures = window.failures.saturating_add(1);
    }

    pub async fn record_success(&self, key: &str) {
        self.state.lock().await.remove(key);
    }
}

pub fn hash_password(password: &str) -> Result<String> {
    validate_password(password)?;
    let mut salt = [0_u8; SALT_BYTES];
    rand::rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt)
        .map_err(|error| anyhow!("cannot encode password salt: {error}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow!("cannot hash password: {error}"))
}

pub fn verify_password(password: &str, encoded_hash: &str) -> bool {
    let Ok(hash) = PasswordHash::new(encoded_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_ok()
}

pub fn new_token() -> String {
    let mut bytes = [0_u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

pub fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn validate_password(password: &str) -> Result<()> {
    ensure!(password.len() >= 12, "password must be at least 12 bytes");
    ensure!(password.len() <= 1024, "password is too long");
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(ALPHABET[(byte >> 4) as usize] as char);
        output.push(ALPHABET[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_verifies_passwords() {
        let encoded = hash_password("correct horse battery staple").unwrap();
        assert!(encoded.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &encoded));
        assert!(!verify_password("wrong password", &encoded));
        assert!(!verify_password("anything", "not-a-password-hash"));
    }

    #[test]
    fn rejects_short_passwords() {
        assert!(hash_password("short").is_err());
    }

    #[test]
    fn creates_independent_tokens_and_stable_hashes() {
        let first = new_token();
        let second = new_token();
        assert_eq!(first.len(), TOKEN_BYTES * 2);
        assert_ne!(first, second);
        assert_eq!(token_hash(&first), token_hash(&first));
        assert_ne!(token_hash(&first), token_hash(&second));
    }

    #[tokio::test]
    async fn limits_repeated_login_failures_and_resets_on_success() {
        let limiter = LoginRateLimiter::new(2, Duration::from_secs(60)).unwrap();
        assert!(limiter.allowed("alice").await);
        limiter.record_failure("alice").await;
        assert!(limiter.allowed("alice").await);
        limiter.record_failure("alice").await;
        assert!(!limiter.allowed("alice").await);
        limiter.record_success("alice").await;
        assert!(limiter.allowed("alice").await);
    }
}
