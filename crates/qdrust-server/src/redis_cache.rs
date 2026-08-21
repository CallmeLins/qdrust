use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::model::AuthenticatedSession;

/// Optional Redis-backed session cache. When `REDIS_URL` is configured, session
/// lookups are served from Redis (TTL = session lifetime) and validated lazily
/// against SQLite as the source of truth.
#[derive(Clone)]
pub struct SessionCache {
    client: Option<redis::Client>,
}

#[derive(Serialize, Deserialize)]
struct CachedSession {
    user: crate::model::User,
    csrf_token_hash: String,
    expires_at: i64,
}

impl SessionCache {
    pub fn from_env() -> Result<Self> {
        match std::env::var("REDIS_URL").ok().filter(|s| !s.is_empty()) {
            Some(url) => {
                let client = redis::Client::open(url).context("invalid REDIS_URL")?;
                Ok(Self {
                    client: Some(client),
                })
            }
            None => Ok(Self { client: None }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.client.is_some()
    }

    async fn run<F, Fut, T>(&self, op: F) -> Option<T>
    where
        F: FnOnce(redis::aio::MultiplexedConnection) -> Fut + Send,
        Fut: std::future::Future<Output = Option<T>> + Send,
        T: Send + 'static,
    {
        let client = self.client.as_ref()?;
        let conn = match client.get_multiplexed_tokio_connection().await {
            Ok(conn) => conn,
            Err(err) => {
                warn!(%err, "redis connection failed");
                return None;
            }
        };
        op(conn).await
    }

    pub async fn get(&self, token_hash: &str) -> Option<AuthenticatedSession> {
        if !self.enabled() {
            return None;
        }
        let key = format!("qdrust:session:{token_hash}");
        let raw: String = match self
            .run(|conn| async move {
                let mut conn = conn;
                redis::cmd("GET")
                    .arg(&key)
                    .query_async::<String>(&mut conn)
                    .await
                    .ok()
            })
            .await
        {
            Some(raw) => raw,
            None => return None,
        };
        let cached: CachedSession = serde_json::from_str(&raw).ok()?;
        if cached.expires_at <= chrono::Utc::now().timestamp() {
            return None;
        }
        Some(AuthenticatedSession {
            user: cached.user,
            csrf_token_hash: cached.csrf_token_hash,
            expires_at: cached.expires_at,
        })
    }

    pub async fn set(&self, token_hash: &str, session: &AuthenticatedSession, ttl_seconds: i64) {
        if !self.enabled() {
            return;
        }
        let key = format!("qdrust:session:{token_hash}");
        let cached = CachedSession {
            user: session.user.clone(),
            csrf_token_hash: session.csrf_token_hash.clone(),
            expires_at: session.expires_at,
        };
        let Ok(raw) = serde_json::to_string(&cached) else {
            return;
        };
        let _ = self
            .run(|conn| async move {
                let mut conn = conn;
                redis::cmd("SETEX")
                    .arg(&key)
                    .arg(ttl_seconds.max(1))
                    .arg(raw)
                    .query_async::<()>(&mut conn)
                    .await
                    .ok()
            })
            .await;
    }

    pub async fn invalidate(&self, token_hash: &str) {
        if !self.enabled() {
            return;
        }
        let key = format!("qdrust:session:{token_hash}");
        let _ = self
            .run(|conn| async move {
                let mut conn = conn;
                redis::cmd("DEL")
                    .arg(&key)
                    .query_async::<()>(&mut conn)
                    .await
                    .ok()
            })
            .await;
    }

    /// Invalidate every cached session for a user (used on password change /
    /// revoke-all flows). Uses a per-user index key.
    pub async fn invalidate_user(&self, user_id: i64, token_hashes: &[String]) {
        for hash in token_hashes {
            self.invalidate(hash).await;
        }
        if !self.enabled() {
            return;
        }
        let key = format!("qdrust:session:user:{user_id}");
        let _ = self
            .run(|conn| async move {
                let mut conn = conn;
                redis::cmd("DEL")
                    .arg(&key)
                    .query_async::<()>(&mut conn)
                    .await
                    .ok()
            })
            .await;
    }
}
