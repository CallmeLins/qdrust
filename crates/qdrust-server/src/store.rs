use std::{path::Path, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow, ensure};
use chrono::Utc;
use sqlx::{
    Column, MySql, MySqlPool, Row, Sqlite, SqlitePool,
    mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow},
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow},
};

use crate::auth::{new_token, token_hash};
use crate::model::{
    AdminUserUpdate, AuthenticatedSession, BatchTaskOperation, CreateNotificationAction,
    CreateNotificationChannel, CreatePluginManifest, CreatePushRequest, CreateTask, CreateTemplate,
    CreateTemplateSubscription, DecidePushRequest, ImportQdHarTemplate, IssuedSession,
    NotificationAction, NotificationChannel, PluginManifest, PushRequest, Run, RunStep,
    SetSiteSetting, SiteSetting, SubscriptionSync, Task, Template, TemplateSubscription,
    UpdateNotificationChannel, UpdatePluginManifest, UpdateQdHarTemplate, UpdateTask,
    UpdateTemplate, UpdateTemplateSubscription, User, UserCredentials,
};
use qdrust_core::{qd_har::QdHar, template::TEMPLATE_SCHEMA_VERSION};

static SQLITE_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");
static MYSQL_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations-mysql");

fn sqlite_options(url: &str) -> Result<SqliteConnectOptions> {
    Ok(SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal))
}

fn mysql_options(url: &str) -> Result<MySqlConnectOptions> {
    Ok(MySqlConnectOptions::from_str(url)?)
}

fn no_parent_check(_url: &str) -> Result<()> {
    Ok(())
}

fn sqlite_username_cmp() -> &'static str {
    "username = ? COLLATE NOCASE"
}

fn mysql_username_cmp() -> &'static str {
    "username = ?"
}

macro_rules! define_store {
    ($module:ident, $db:ty, $pool:ty, $pool_options:ty, $options_builder:path, $migrator:expr,
     $row:ty, $last_id_sql:expr, $username_cmp:path, $setting_upsert:expr, $parent_check:path) => {
        #[allow(clippy::all)]
        pub mod $module {
            use super::*;

            #[derive(Clone)]
            pub struct Store {
                pub(crate) pool: $pool,
            }

            impl Store {
                pub async fn connect(
                    url: &str,
                    min_connections: u32,
                    max_connections: u32,
                ) -> Result<Self> {
                    Self::connect_with_timeouts(
                        url,
                        min_connections,
                        max_connections,
                        Duration::from_secs(30),
                        Duration::from_secs(600),
                    )
                    .await
                }

                pub async fn connect_with_timeouts(
                    url: &str,
                    min_connections: u32,
                    max_connections: u32,
                    acquire_timeout: Duration,
                    idle_timeout: Duration,
                ) -> Result<Self> {
                    $parent_check(url)?;
                    let options = $options_builder(url)?;
                    let pool = <$pool_options>::new()
                        .min_connections(min_connections)
                        .max_connections(max_connections)
                        .acquire_timeout(acquire_timeout)
                        .idle_timeout(idle_timeout)
                        .connect_with(options)
                        .await?;
                    let store = Self { pool };
                    $migrator.run(&store.pool).await?;
                    Ok(store)
                }

                async fn last_insert_id(
                    &self,
                    conn: &mut <$db as sqlx::Database>::Connection,
                ) -> Result<i64> {
                    Ok(sqlx::query_scalar($last_id_sql).fetch_one(conn).await?)
                }

    pub async fn ready(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        role: &str,
    ) -> Result<User> {
        validate_username(username)?;
        ensure!(
            password_hash.starts_with("$argon2id$"),
            "invalid password hash"
        );
        ensure!(matches!(role, "admin" | "user"), "invalid user role");
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO users(username, password_hash, role, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(username.trim())
        .bind(password_hash)
        .bind(role)
        .bind(now)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_user(id)
            .await?
            .context("created user disappeared")
    }

    pub async fn create_first_admin(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<Option<User>> {
        validate_username(username)?;
        ensure!(
            password_hash.starts_with("$argon2id$"),
            "invalid password hash"
        );
        let mut transaction = self.pool.begin().await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&mut *transaction)
            .await?;
        if count != 0 {
            transaction.rollback().await?;
            return Ok(None);
        }
        let now = Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO users(username,password_hash,role,created_at,updated_at)
             VALUES (?,?,'admin',?,?)",
        )
        .bind(username.trim())
        .bind(password_hash)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        let id = self.last_insert_id(&mut *transaction).await?;
        transaction.commit().await?;
        self.get_user(id).await
    }

    pub async fn get_user(&self, id: i64) -> Result<Option<User>> {
        let row = sqlx::query(&format!("{USER_FIELDS} WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(user_from_row).transpose()
    }

    pub async fn credentials_by_username(&self, username: &str) -> Result<Option<UserCredentials>> {
        let row = sqlx::query(&format!(
            "SELECT id,username,password_hash,role,disabled,email,email_verified,session_version,created_at,updated_at FROM users WHERE {}",
            $username_cmp()
        ))
        .bind(username.trim())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(credentials_from_row).transpose()
    }

    pub async fn create_session(&self, user_id: i64, ttl: Duration) -> Result<IssuedSession> {
        ensure!(!ttl.is_zero(), "session TTL must be positive");
        let credentials = self
            .credentials_by_id(user_id)
            .await?
            .context("user not found")?;
        ensure!(!credentials.user.disabled, "user is disabled");
        let session_token = new_token();
        let csrf_token = new_token();
        let now = Utc::now().timestamp();
        let ttl = i64::try_from(ttl.as_secs()).context("session TTL is too large")?;
        let expires_at = now.checked_add(ttl).context("session expiry overflow")?;
        sqlx::query(
            "INSERT INTO sessions(token_hash,user_id,csrf_token_hash,session_version,created_at,last_seen_at,expires_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(token_hash(&session_token))
        .bind(user_id)
        .bind(token_hash(&csrf_token))
        .bind(credentials.session_version)
        .bind(now)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(IssuedSession {
            session_token,
            csrf_token,
            expires_at,
        })
    }

    pub async fn authenticate_session(
        &self,
        session_token: &str,
    ) -> Result<Option<AuthenticatedSession>> {
        let row = sqlx::query(
            "SELECT u.id,u.username,u.role,u.disabled,u.email,u.email_verified,u.created_at,u.updated_at,
                    s.csrf_token_hash,s.expires_at
             FROM sessions s JOIN users u ON u.id=s.user_id
             WHERE s.token_hash=? AND s.expires_at>? AND s.session_version=u.session_version
                   AND u.disabled=0",
        )
        .bind(token_hash(session_token))
        .bind(Utc::now().timestamp())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(authenticated_session_from_row).transpose()
    }

    pub async fn revoke_session(&self, session_token: &str) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM sessions WHERE token_hash=?")
            .bind(token_hash(session_token))
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn revoke_all_sessions(&self, user_id: i64) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("UPDATE users SET session_version=session_version+1,updated_at=? WHERE id=?")
            .bind(Utc::now().timestamp())
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE user_id=?")
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn change_password(&self, user_id: i64, password_hash: &str) -> Result<bool> {
        ensure!(
            password_hash.starts_with("$argon2id$"),
            "invalid password hash"
        );
        let mut transaction = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE users SET password_hash=?,session_version=session_version+1,updated_at=? WHERE id=? AND disabled=0",
        )
        .bind(password_hash)
        .bind(Utc::now().timestamp())
        .bind(user_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated == 0 {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query("DELETE FROM sessions WHERE user_id=?")
            .bind(user_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(true)
    }

    pub async fn record_audit(
        &self,
        actor_user_id: Option<i64>,
        action: &str,
        resource_type: Option<&str>,
        resource_id: Option<i64>,
        request_id: Option<&str>,
        details: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_logs(actor_user_id,action,resource_type,resource_id,request_id,details,created_at)
             VALUES (?,?,?,?,?,?,?)",
        )
        .bind(actor_user_id)
        .bind(action)
        .bind(resource_type)
        .bind(resource_id)
        .bind(request_id)
        .bind(serde_json::to_string(details)?)
        .bind(Utc::now().timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn purge_expired_sessions(&self) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM sessions WHERE expires_at<=?")
            .bind(Utc::now().timestamp())
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    async fn credentials_by_id(&self, id: i64) -> Result<Option<UserCredentials>> {
        let row = sqlx::query(
            "SELECT id,username,password_hash,role,disabled,email,email_verified,session_version,created_at,updated_at
             FROM users WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(credentials_from_row).transpose()
    }

    pub async fn create(&self, input: CreateTask) -> Result<Task> {
        self.create_with_owner(None, input).await
    }

    pub async fn create_for_owner(&self, owner_id: i64, input: CreateTask) -> Result<Task> {
        self.create_with_owner(Some(owner_id), input).await
    }

    async fn create_with_owner(&self, owner_id: Option<i64>, input: CreateTask) -> Result<Task> {
        validate(&input.name, &input.cron, &input.url, input.timezone.as_deref())?;
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO tasks(name, cron, method, url, headers, body, disabled, created_at, updated_at, owner_id, template_id, grp, timeout_seconds, retry_count, retry_interval_seconds, priority, timezone, random_delay_max_seconds, variables)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.name)
        .bind(input.cron)
        .bind(input.method.unwrap_or_else(|| "GET".into()).to_uppercase())
        .bind(input.url)
        .bind(serde_json::to_string(&input.headers)?)
        .bind(input.body)
        .bind(input.disabled)
        .bind(now)
        .bind(now)
        .bind(owner_id)
        .bind(input.template_id)
        .bind(input.grp.as_deref())
        .bind(input.timeout_seconds)
        .bind(input.retry_count)
        .bind(input.retry_interval_seconds)
        .bind(input.priority.unwrap_or(0))
        .bind(input.timezone.as_deref())
        .bind(input.random_delay_max_seconds.unwrap_or(0))
        .bind(input.variables.map(|v| v.to_string()))
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get(id)
            .await?
            .context("created task disappeared")
    }

    pub async fn list(&self) -> Result<Vec<Task>> {
        let rows = sqlx::query(TASK_FIELDS).fetch_all(&self.pool).await?;
        rows.into_iter().map(task_from_row).collect()
    }

    pub async fn list_for_owner(&self, owner_id: i64) -> Result<Vec<Task>> {
        let rows = sqlx::query(&format!("{TASK_FIELDS} WHERE owner_id = ?"))
            .bind(owner_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(task_from_row).collect()
    }

    pub async fn list_for_owner_with_group(
        &self,
        owner_id: i64,
        grp: Option<&str>,
    ) -> Result<Vec<Task>> {
        let mut statement = TASK_FIELDS.to_string();
        if grp.is_some() {
            statement.push_str(" WHERE owner_id=? AND grp=?");
        } else {
            statement.push_str(" WHERE owner_id=?");
        }
        statement.push_str(" ORDER BY grp, id");
        let mut query = sqlx::query(&statement).bind(owner_id);
        if let Some(grp) = grp {
            query = query.bind(grp);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter().map(task_from_row).collect()
    }

    pub async fn get(&self, id: i64) -> Result<Option<Task>> {
        let row = sqlx::query(&format!("{TASK_FIELDS} WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(task_from_row).transpose()
    }

    pub async fn get_for_owner(&self, id: i64, owner_id: i64) -> Result<Option<Task>> {
        let row = sqlx::query(&format!("{TASK_FIELDS} WHERE id = ? AND owner_id = ?"))
            .bind(id)
            .bind(owner_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(task_from_row).transpose()
    }

    pub async fn update(&self, id: i64, input: UpdateTask) -> Result<Option<Task>> {
        self.update_for_optional_owner(id, None, input).await
    }

    pub async fn update_for_owner(
        &self,
        id: i64,
        owner_id: i64,
        input: UpdateTask,
    ) -> Result<Option<Task>> {
        self.update_for_optional_owner(id, Some(owner_id), input)
            .await
    }

    async fn update_for_optional_owner(
        &self,
        id: i64,
        owner_id: Option<i64>,
        input: UpdateTask,
    ) -> Result<Option<Task>> {
        let current = match owner_id {
            Some(owner_id) => self.get_for_owner(id, owner_id).await?,
            None => self.get(id).await?,
        };
        let Some(current) = current else {
            return Ok(None);
        };
        let name = input.name.unwrap_or(current.name);
        let cron = input.cron.unwrap_or(current.cron);
        let url = input.url.unwrap_or(current.url);
        validate(&name, &cron, &url, input.timezone.as_ref().and_then(|tz| tz.as_deref()))?;
        let headers = input
            .headers
            .map(serde_json::Value::Object)
            .unwrap_or(current.headers);
        let grp = match input.grp {
            Some(grp) => grp,
            None => current.grp,
        };
        sqlx::query(
            "UPDATE tasks SET name=?, cron=?, method=?, url=?, headers=?, body=?, disabled=?, template_id=?, grp=?, timeout_seconds=?, retry_count=?, retry_interval_seconds=?, priority=?, timezone=?, random_delay_max_seconds=?, variables=?, updated_at=? WHERE id=?",
        )
        .bind(name)
        .bind(cron)
        .bind(input.method.unwrap_or(current.method).to_uppercase())
        .bind(url)
        .bind(serde_json::to_string(&headers)?)
        .bind(input.body.or(current.body))
        .bind(input.disabled.unwrap_or(current.disabled))
        .bind(input.template_id.or(current.template_id))
        .bind(grp.as_deref())
        .bind(merge_optional(input.timeout_seconds, current.timeout_seconds))
        .bind(merge_optional(input.retry_count, current.retry_count))
        .bind(merge_optional(input.retry_interval_seconds, current.retry_interval_seconds))
        .bind(merge_optional(input.priority, current.priority).unwrap_or(0))
        .bind(merge_optional(input.timezone, current.timezone).as_deref())
        .bind(merge_optional(input.random_delay_max_seconds, current.random_delay_max_seconds).unwrap_or(0))
        .bind(
            merge_optional(input.variables, current.variables)
                .map(|v| v.to_string()),
        )
        .bind(Utc::now().timestamp())
        .bind(id)
        .execute(&self.pool)
        .await?;
        self.get(id).await
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM tasks WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn delete_for_owner(&self, id: i64, owner_id: i64) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM tasks WHERE id=? AND owner_id=?")
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn record_run(
        &self,
        id: i64,
        status: Option<u16>,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE tasks SET last_run_at=?, last_status=?, last_error=? WHERE id=?")
            .bind(Utc::now().timestamp())
            .bind(status.map(i64::from))
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn start_run(&self, task_id: i64) -> Result<Run> {
        let started_at = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO runs(task_id,status,started_at,created_at,attempt)
             VALUES (?,'running',?,?,1)",
        )
        .bind(task_id)
        .bind(started_at)
        .bind(started_at)
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_run(id)
            .await?
            .context("created run disappeared")
    }

    pub async fn enqueue_run(&self, task_id: i64) -> Result<Option<Run>> {
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        let result = sqlx::query(
            "INSERT INTO runs(task_id,status,created_at,attempt)
             SELECT ?, 'pending', ?, 0
             WHERE NOT EXISTS (
                SELECT 1 FROM runs WHERE task_id=? AND status IN ('pending','leased','running')
             )",
        )
        .bind(task_id)
        .bind(now)
        .bind(task_id)
        .execute(&mut *conn)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_run(id).await
    }

    /// Enqueue a run that only becomes claimable after `delay_seconds` (QD-style
    /// random delay: the jitter is drawn once at enqueue time and stored in
    /// `run_after`). The one-active-run-per-task guard is preserved.
    pub async fn enqueue_delayed_run(&self, task_id: i64, delay_seconds: i64) -> Result<Option<Run>> {
        let now = Utc::now().timestamp();
        let run_after = now + delay_seconds.max(0);
        let mut conn = self.pool.acquire().await?;
        let result = sqlx::query(
            "INSERT INTO runs(task_id,status,created_at,run_after,attempt)
             SELECT ?, 'pending', ?, ?, 0
             WHERE NOT EXISTS (
                SELECT 1 FROM runs WHERE task_id=? AND status IN ('pending','leased','running')
             )",
        )
        .bind(task_id)
        .bind(now)
        .bind(run_after)
        .bind(task_id)
        .execute(&mut *conn)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_run(id).await
    }

    /// Schedule a delayed retry for a failed run. `retry_of` tracks the original
    /// run so the retry chain can be counted; `run_after` delays the claim.
    /// The one-active-run-per-task guard is preserved (skips if a new run is
    /// already active).
    pub async fn schedule_retry(&self, task_id: i64, retry_of: i64, delay_seconds: i64) -> Result<Option<Run>> {
        let now = Utc::now().timestamp();
        let run_after = now + delay_seconds.max(1);
        let mut conn = self.pool.acquire().await?;
        let result = sqlx::query(
            "INSERT INTO runs(task_id,status,created_at,run_after,retry_of,attempt)
             SELECT ?, 'pending', ?, ?, ?, 0
             WHERE NOT EXISTS (
                SELECT 1 FROM runs WHERE task_id=? AND status IN ('pending','leased','running')
             )",
        )
        .bind(task_id)
        .bind(now)
        .bind(run_after)
        .bind(retry_of)
        .bind(task_id)
        .execute(&mut *conn)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_run(id).await
    }

    /// Number of retries already scheduled for a retry chain (runs whose
    /// retry_of points at the original run).
    pub async fn count_retries(&self, original_run_id: i64) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM runs WHERE retry_of=?")
            .bind(original_run_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("count")?)
    }

    pub async fn claim_run(&self, worker: &str, lease_seconds: i64) -> Result<Option<Run>> {
        let now = Utc::now().timestamp();
        let expires = now + lease_seconds.max(1);
        let mut tx = self.pool.begin().await?;
        // Claim pending runs (or expired leases). Delayed retry runs (run_after)
        // become claimable once their delay elapses; higher task priority claims
        // first.
        let row = sqlx::query(
            "SELECT r.id FROM runs r LEFT JOIN tasks t ON t.id=r.task_id \
             WHERE r.cancel_requested=0 AND (r.status='pending' OR (r.status IN ('leased','running') AND r.lease_expires_at<=?)) \
             AND (r.run_after IS NULL OR r.run_after<=?) \
             ORDER BY COALESCE(t.priority,0) DESC, r.created_at, r.id LIMIT 1",
        )
        .bind(now)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        let id: i64 = row.try_get("id")?;
        let updated = sqlx::query("UPDATE runs SET status='leased',lease_owner=?,lease_expires_at=?,attempt=attempt+1 WHERE id=? AND cancel_requested=0 AND (status='pending' OR (status IN ('leased','running') AND lease_expires_at<=?))")
            .bind(worker).bind(expires).bind(id).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        if updated.rows_affected() == 0 {
            return Ok(None);
        }
        self.get_run(id).await
    }

    pub async fn start_leased_run(&self, run_id: i64, worker: &str) -> Result<bool> {
        Ok(sqlx::query("UPDATE runs SET status='running',started_at=? WHERE id=? AND status='leased' AND lease_owner=? AND cancel_requested=0")
            .bind(Utc::now().timestamp()).bind(run_id).bind(worker).execute(&self.pool).await?.rows_affected() > 0)
    }

    pub async fn renew_run(&self, run_id: i64, worker: &str, lease_seconds: i64) -> Result<bool> {
        Ok(sqlx::query("UPDATE runs SET lease_expires_at=? WHERE id=? AND status IN ('leased','running') AND lease_owner=?")
            .bind(Utc::now().timestamp() + lease_seconds.max(1)).bind(run_id).bind(worker).execute(&self.pool).await?.rows_affected() > 0)
    }

    pub async fn cancel_run(&self, run_id: i64) -> Result<bool> {
        let now = Utc::now().timestamp();
        let result = sqlx::query("UPDATE runs SET status=CASE WHEN status='pending' THEN 'cancelled' ELSE status END,cancel_requested=1,finished_at=CASE WHEN status='pending' THEN ? ELSE finished_at END WHERE id=? AND status IN ('pending','leased','running')")
            .bind(now).bind(run_id).execute(&self.pool).await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn recover_expired_runs(&self) -> Result<u64> {
        let now = Utc::now().timestamp();
        Ok(sqlx::query("UPDATE runs SET status=CASE WHEN cancel_requested=1 THEN 'cancelled' ELSE 'pending' END,lease_owner=NULL,lease_expires_at=NULL,finished_at=CASE WHEN cancel_requested=1 THEN ? ELSE finished_at END WHERE status IN ('leased','running') AND lease_expires_at<=?")
            .bind(now).bind(now).execute(&self.pool).await?.rows_affected())
    }

    pub async fn finish_run(
        &self,
        run_id: i64,
        http_status: Option<u16>,
        error: Option<&str>,
    ) -> Result<()> {
        let status = if error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        sqlx::query(
            "UPDATE runs SET status=CASE WHEN cancel_requested=1 THEN 'cancelled' ELSE ? END, http_status=?, error=?, finished_at=?, lease_owner=NULL, lease_expires_at=NULL WHERE id=? AND status IN ('running','leased')",
        )
        .bind(status)
        .bind(http_status.map(i64::from))
        .bind(error)
        .bind(Utc::now().timestamp())
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persist the QD-style log line (the final __log__ value) for a run.
    pub async fn record_run_log(&self, run_id: i64, log: &str) -> Result<()> {
        sqlx::query("UPDATE runs SET log=? WHERE id=?")
            .bind(log)
            .bind(run_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete a single run (run_steps cascade via FK).
    pub async fn delete_run(&self, run_id: i64) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM runs WHERE id=?")
            .bind(run_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    /// Clear the run history of every task owned by `owner_id`; returns the
    /// number of deleted runs.
    pub async fn delete_runs_for_owner(&self, owner_id: i64) -> Result<u64> {
        Ok(sqlx::query(
            "DELETE FROM runs WHERE task_id IN (SELECT id FROM tasks WHERE owner_id=?)",
        )
        .bind(owner_id)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Clear the whole run history (admin).
    pub async fn delete_all_runs(&self) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM runs")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub async fn record_run_step(&self, step: &RunStep) -> Result<()> {
        sqlx::query(
            "INSERT INTO run_steps(run_id,step_index,name,status,http_status,body_size,error,started_at,finished_at)
             VALUES (?,?,?,?,?,?,?,?,?)",
        )
        .bind(step.run_id)
        .bind(step.step_index)
        .bind(&step.name)
        .bind(&step.status)
        .bind(step.http_status)
        .bind(step.body_size)
        .bind(&step.error)
        .bind(step.started_at)
        .bind(step.finished_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_run_steps(&self, run_id: i64) -> Result<Vec<RunStep>> {
        let rows = sqlx::query(
            "SELECT id,run_id,step_index,name,status,http_status,body_size,error,started_at,finished_at
             FROM run_steps WHERE run_id=? ORDER BY step_index",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(run_step_from_row).collect()
    }

    pub async fn list_run_steps_for_owner(
        &self,
        run_id: i64,
        owner_id: i64,
    ) -> Result<Option<Vec<RunStep>>> {
        let owns_run: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM runs r JOIN tasks t ON t.id=r.task_id
                           WHERE r.id=? AND t.owner_id=?)",
        )
        .bind(run_id)
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?;
        if !owns_run {
            return Ok(None);
        }
        Ok(Some(self.list_run_steps(run_id).await?))
    }

    pub async fn get_run(&self, id: i64) -> Result<Option<Run>> {
        let row = sqlx::query(&format!("{RUN_FIELDS} WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(run_from_row).transpose()
    }

    pub async fn list_task_runs(&self, task_id: i64) -> Result<Vec<Run>> {
        let rows = sqlx::query(&format!(
            "{RUN_FIELDS} WHERE task_id = ? ORDER BY created_at DESC, id DESC"
        ))
        .bind(task_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(run_from_row).collect()
    }

    pub async fn list_task_runs_for_owner(
        &self,
        task_id: i64,
        owner_id: i64,
    ) -> Result<Option<Vec<Run>>> {
        if self.get_for_owner(task_id, owner_id).await?.is_none() {
            return Ok(None);
        }
        Ok(Some(self.list_task_runs(task_id).await?))
    }

    pub async fn create_notification_channel(
        &self,
        owner_id: i64,
        input: CreateNotificationChannel,
    ) -> Result<NotificationChannel> {
        validate_notification(&input.name, &input.kind, &input.config)?;
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query("INSERT INTO notification_channels(owner_id,name,kind,config,enabled,created_at,updated_at) VALUES (?,?,?,?,?,?,?)")
            .bind(owner_id).bind(input.name.trim()).bind(input.kind).bind(serde_json::to_string(&input.config)?).bind(input.enabled).bind(now).bind(now).execute(&mut *conn).await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_notification_channel(id, owner_id)
            .await?
            .context("created notification channel disappeared")
    }

    pub async fn list_notification_channels(
        &self,
        owner_id: i64,
    ) -> Result<Vec<NotificationChannel>> {
        let rows = sqlx::query("SELECT id,name,kind,config,enabled,created_at,updated_at FROM notification_channels WHERE owner_id=? ORDER BY id")
            .bind(owner_id).fetch_all(&self.pool).await?;
        rows.into_iter().map(notification_from_row).collect()
    }

    pub async fn get_notification_channel(
        &self,
        id: i64,
        owner_id: i64,
    ) -> Result<Option<NotificationChannel>> {
        let row = sqlx::query("SELECT id,name,kind,config,enabled,created_at,updated_at FROM notification_channels WHERE id=? AND owner_id=?")
            .bind(id).bind(owner_id).fetch_optional(&self.pool).await?;
        row.map(notification_from_row).transpose()
    }

    pub async fn update_notification_channel(
        &self,
        id: i64,
        owner_id: i64,
        input: UpdateNotificationChannel,
    ) -> Result<Option<NotificationChannel>> {
        let Some(current) = self.get_notification_channel(id, owner_id).await? else {
            return Ok(None);
        };
        let name = input.name.unwrap_or(current.name);
        let config = input.config.unwrap_or(current.config);
        validate_notification(&name, &current.kind, &config)?;
        sqlx::query("UPDATE notification_channels SET name=?,config=?,enabled=?,updated_at=? WHERE id=? AND owner_id=?")
            .bind(name.trim()).bind(serde_json::to_string(&config)?).bind(input.enabled.unwrap_or(current.enabled)).bind(Utc::now().timestamp()).bind(id).bind(owner_id).execute(&self.pool).await?;
        self.get_notification_channel(id, owner_id).await
    }

    pub async fn delete_notification_channel(&self, id: i64, owner_id: i64) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM notification_channels WHERE id=? AND owner_id=?")
                .bind(id)
                .bind(owner_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn create_notification_action(
        &self,
        task_id: i64,
        owner_id: i64,
        input: CreateNotificationAction,
    ) -> Result<Option<NotificationAction>> {
        ensure!(
            matches!(input.event.as_str(), "success" | "failure" | "always"),
            "invalid notification event"
        );
        let owns: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tasks t JOIN notification_channels c ON c.owner_id=t.owner_id WHERE t.id=? AND t.owner_id=? AND c.id=?)")
            .bind(task_id).bind(owner_id).bind(input.channel_id).fetch_one(&self.pool).await?;
        if !owns {
            return Ok(None);
        }
        let mut conn = self.pool.acquire().await?;
        sqlx::query("INSERT INTO notification_actions(task_id,channel_id,event,created_at) VALUES (?,?,?,?)")
            .bind(task_id).bind(input.channel_id).bind(input.event).bind(Utc::now().timestamp()).execute(&mut *conn).await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_notification_action(id, owner_id)
            .await
    }

    pub async fn list_notification_actions(
        &self,
        task_id: i64,
        owner_id: i64,
    ) -> Result<Option<Vec<NotificationAction>>> {
        if self.get_for_owner(task_id, owner_id).await?.is_none() {
            return Ok(None);
        }
        let rows = sqlx::query("SELECT a.id,a.task_id,a.channel_id,a.event,a.created_at FROM notification_actions a WHERE a.task_id=? ORDER BY a.id")
            .bind(task_id).fetch_all(&self.pool).await?;
        Ok(Some(
            rows.into_iter()
                .map(notification_action_from_row)
                .collect::<Result<Vec<_>>>()?,
        ))
    }

    pub async fn delete_notification_action(&self, id: i64, owner_id: i64) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM notification_actions WHERE id=? AND task_id IN (SELECT id FROM tasks WHERE owner_id=?)")
            .bind(id).bind(owner_id).execute(&self.pool).await?.rows_affected() > 0)
    }

    pub async fn notification_channels_for_event(
        &self,
        task_id: i64,
        event: &str,
    ) -> Result<Vec<NotificationChannel>> {
        ensure!(
            matches!(event, "success" | "failure"),
            "invalid notification event"
        );
        let rows = sqlx::query("SELECT c.id,c.name,c.kind,c.config,c.enabled,c.created_at,c.updated_at FROM notification_channels c JOIN notification_actions a ON a.channel_id=c.id WHERE a.task_id=? AND c.enabled=1 AND a.event IN (?, 'always') ORDER BY a.id")
            .bind(task_id).bind(event).fetch_all(&self.pool).await?;
        rows.into_iter().map(notification_from_row).collect()
    }

    pub async fn create_plugin(
        &self,
        owner_id: i64,
        input: CreatePluginManifest,
    ) -> Result<PluginManifest> {
        validate_plugin(&input.name, &input.command, &input.config)?;
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query("INSERT INTO plugins(owner_id,name,command,config,enabled,created_at,updated_at) VALUES(?,?,?,?,?,?,?)").bind(owner_id).bind(input.name.trim()).bind(input.command).bind(serde_json::to_string(&input.config)?).bind(input.enabled).bind(now).bind(now).execute(&mut *conn).await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_plugin(id, owner_id)
            .await?
            .context("created plugin disappeared")
    }
    pub async fn list_plugins(&self, owner_id: i64) -> Result<Vec<PluginManifest>> {
        let rows=sqlx::query("SELECT id,name,command,config,enabled,created_at,updated_at FROM plugins WHERE owner_id=? ORDER BY id").bind(owner_id).fetch_all(&self.pool).await?;
        rows.into_iter().map(plugin_from_row).collect()
    }
    pub async fn get_plugin(&self, id: i64, owner_id: i64) -> Result<Option<PluginManifest>> {
        let row=sqlx::query("SELECT id,name,command,config,enabled,created_at,updated_at FROM plugins WHERE id=? AND owner_id=?").bind(id).bind(owner_id).fetch_optional(&self.pool).await?;
        row.map(plugin_from_row).transpose()
    }
    pub async fn update_plugin(
        &self,
        id: i64,
        owner_id: i64,
        input: UpdatePluginManifest,
    ) -> Result<Option<PluginManifest>> {
        let Some(current) = self.get_plugin(id, owner_id).await? else {
            return Ok(None);
        };
        let name = input.name.unwrap_or(current.name);
        let command = input.command.unwrap_or(current.command);
        let config = input.config.unwrap_or(current.config);
        validate_plugin(&name, &command, &config)?;
        sqlx::query("UPDATE plugins SET name=?,command=?,config=?,enabled=?,updated_at=? WHERE id=? AND owner_id=?").bind(name.trim()).bind(command).bind(serde_json::to_string(&config)?).bind(input.enabled.unwrap_or(current.enabled)).bind(Utc::now().timestamp()).bind(id).bind(owner_id).execute(&self.pool).await?;
        self.get_plugin(id, owner_id).await
    }
    pub async fn delete_plugin(&self, id: i64, owner_id: i64) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM plugins WHERE id=? AND owner_id=?")
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    async fn get_notification_action(
        &self,
        id: i64,
        owner_id: i64,
    ) -> Result<Option<NotificationAction>> {
        let row = sqlx::query("SELECT a.id,a.task_id,a.channel_id,a.event,a.created_at FROM notification_actions a JOIN tasks t ON t.id=a.task_id WHERE a.id=? AND t.owner_id=?")
            .bind(id).bind(owner_id).fetch_optional(&self.pool).await?;
        row.map(notification_action_from_row).transpose()
    }

    pub async fn create_template(&self, input: CreateTemplate) -> Result<Template> {
        self.create_template_with_owner(None, input).await
    }

    pub async fn create_template_for_owner(
        &self,
        owner_id: i64,
        input: CreateTemplate,
    ) -> Result<Template> {
        self.create_template_with_owner(Some(owner_id), input).await
    }

    async fn create_template_with_owner(
        &self,
        owner_id: Option<i64>,
        input: CreateTemplate,
    ) -> Result<Template> {
        validate_template_name(&input.name)?;
        input.definition.validate()?;
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO templates(name, description, schema_version, definition, created_at, updated_at, owner_id, grp)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(input.name)
        .bind(input.description)
        .bind(i64::from(TEMPLATE_SCHEMA_VERSION))
        .bind(serde_json::to_string(&input.definition)?)
        .bind(now)
        .bind(now)
        .bind(owner_id)
        .bind(input.grp.as_deref())
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_template(id)
            .await?
            .context("created template disappeared")
    }

    pub async fn import_qd_har(&self, input: ImportQdHarTemplate) -> Result<Template> {
        self.import_qd_har_with_owner(None, input).await
    }

    pub async fn import_qd_har_for_owner(
        &self,
        owner_id: i64,
        input: ImportQdHarTemplate,
    ) -> Result<Template> {
        self.import_qd_har_with_owner(Some(owner_id), input).await
    }

    async fn import_qd_har_with_owner(
        &self,
        owner_id: Option<i64>,
        input: ImportQdHarTemplate,
    ) -> Result<Template> {
        validate_template_name(&input.name)?;
        QdHar::parse(input.har.clone())?;
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO templates(name, description, schema_version, definition, source_format, source, created_at, updated_at, owner_id)
             VALUES (?, ?, 1, '{}', 'qd_har', ?, ?, ?, ?)",
        )
        .bind(input.name)
        .bind(input.description)
        .bind(serde_json::to_string(&input.har)?)
        .bind(now)
        .bind(now)
        .bind(owner_id)
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_template(id)
            .await?
            .context("imported template disappeared")
    }

    pub async fn update_qd_har_for_owner(
        &self,
        id: i64,
        owner_id: i64,
        input: UpdateQdHarTemplate,
    ) -> Result<Option<Template>> {
        validate_template_name(&input.name)?;
        QdHar::parse(input.har.clone())?;
        let changed=sqlx::query("UPDATE templates SET name=?,description=?,source=?,updated_at=? WHERE id=? AND owner_id=? AND source_format='qd_har'").bind(input.name).bind(input.description).bind(serde_json::to_string(&input.har)?).bind(Utc::now().timestamp()).bind(id).bind(owner_id).execute(&self.pool).await?.rows_affected();
        if changed == 0 {
            return Ok(None);
        }
        self.get_template_for_owner(id, owner_id).await
    }

    pub async fn list_templates(&self) -> Result<Vec<Template>> {
        let rows = sqlx::query(TEMPLATE_FIELDS).fetch_all(&self.pool).await?;
        rows.into_iter().map(template_from_row).collect()
    }

    pub async fn list_templates_for_owner(&self, owner_id: i64) -> Result<Vec<Template>> {
        let rows = sqlx::query(&format!("{TEMPLATE_FIELDS} WHERE owner_id = ?"))
            .bind(owner_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(template_from_row).collect()
    }

    pub async fn get_template(&self, id: i64) -> Result<Option<Template>> {
        let row = sqlx::query(&format!("{TEMPLATE_FIELDS} WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(template_from_row).transpose()
    }

    pub async fn get_template_for_owner(&self, id: i64, owner_id: i64) -> Result<Option<Template>> {
        let row = sqlx::query(&format!("{TEMPLATE_FIELDS} WHERE id = ? AND owner_id = ?"))
            .bind(id)
            .bind(owner_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(template_from_row).transpose()
    }

    pub async fn update_template(
        &self,
        id: i64,
        input: UpdateTemplate,
    ) -> Result<Option<Template>> {
        self.update_template_with_owner(id, None, input).await
    }

    async fn update_template_with_owner(
        &self,
        id: i64,
        owner_id: Option<i64>,
        input: UpdateTemplate,
    ) -> Result<Option<Template>> {
        let current = match owner_id {
            Some(owner_id) => self.get_template_for_owner(id, owner_id).await?,
            None => self.get_template(id).await?,
        };
        let Some(current) = current else {
            return Ok(None);
        };
        let name = input.name.unwrap_or(current.name);
        validate_template_name(&name)?;
        ensure!(
            current.source_format == "native_v1",
            "QD HAR templates must be edited through the HAR API"
        );
        let definition = input
            .definition
            .or(current.definition)
            .context("native template definition is missing")?;
        definition.validate()?;
        let owner_clause = if owner_id.is_some() {
            " AND owner_id=?"
        } else {
            ""
        };
        let grp = match input.grp {
            Some(grp) => grp,
            None => current.grp,
        };
        let statement = format!(
            "UPDATE templates SET name=?, description=?, schema_version=?, definition=?, grp=?, updated_at=? WHERE id=?{owner_clause}"
        );
        let mut query = sqlx::query(&statement)
            .bind(name)
            .bind(input.description.or(current.description))
            .bind(i64::from(TEMPLATE_SCHEMA_VERSION))
            .bind(serde_json::to_string(&definition)?)
            .bind(grp.as_deref())
            .bind(Utc::now().timestamp())
            .bind(id);
        if let Some(owner_id) = owner_id {
            query = query.bind(owner_id);
        }
        let result = query.execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        match owner_id {
            Some(owner_id) => self.get_template_for_owner(id, owner_id).await,
            None => self.get_template(id).await,
        }
    }

    pub async fn update_template_for_owner(
        &self,
        id: i64,
        owner_id: i64,
        input: UpdateTemplate,
    ) -> Result<Option<Template>> {
        self.update_template_with_owner(id, Some(owner_id), input)
            .await
    }

    pub async fn delete_template(&self, id: i64) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM templates WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn delete_template_for_owner(&self, id: i64, owner_id: i64) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM templates WHERE id=? AND owner_id=?")
                .bind(id)
                .bind(owner_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn set_template_published(
        &self,
        id: i64,
        owner_id: i64,
        published: bool,
    ) -> Result<bool> {
        Ok(
            sqlx::query("UPDATE templates SET published=?,updated_at=? WHERE id=? AND owner_id=?")
                .bind(published)
                .bind(Utc::now().timestamp())
                .bind(id)
                .bind(owner_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn list_public_templates(&self) -> Result<Vec<Template>> {
        let rows = sqlx::query(&format!(
            "{TEMPLATE_FIELDS} WHERE published=1 ORDER BY updated_at DESC,id DESC"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(template_from_row).collect()
    }

    pub async fn copy_public_template(&self, id: i64, owner_id: i64) -> Result<Option<Template>> {
        let row=sqlx::query("SELECT name,description,schema_version,source_format,definition,source FROM templates WHERE id=? AND published=1").bind(id).fetch_optional(&self.pool).await?;
        let Some(row) = row else { return Ok(None) };
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query("INSERT INTO templates(name,description,schema_version,source_format,definition,source,created_at,updated_at,owner_id,published) VALUES(?,?,?,?,?,?,?,?,?,0)").bind(format!("{} (copy)",row.try_get::<String,_>("name")?)).bind(row.try_get::<Option<String>,_>("description")?).bind(row.try_get::<i64,_>("schema_version")?).bind(row.try_get::<String,_>("source_format")?).bind(row.try_get::<String,_>("definition")?).bind(row.try_get::<Option<String>,_>("source")?).bind(now).bind(now).bind(owner_id).execute(&mut *conn).await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_template_for_owner(id, owner_id)
            .await
    }

    // ---- Task grouping & batch operations ----

    pub async fn list_groups_for_owner(&self, owner_id: i64) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT DISTINCT grp FROM tasks WHERE owner_id=? AND grp IS NOT NULL AND grp<>'' ORDER BY grp",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row.try_get::<String, _>("grp")?);
        }
        Ok(groups)
    }

    /// Apply a batch operation to many owned tasks at once.
    pub async fn batch_operations_for_owner(
        &self,
        owner_id: i64,
        input: &BatchTaskOperation,
    ) -> Result<usize> {
        if input.ids.is_empty() {
            return Ok(0);
        }
        let ids = input
            .ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let updated = match input.action.as_str() {
            "enable" => sqlx::query(&format!(
                "UPDATE tasks SET disabled=0,updated_at=? WHERE owner_id=? AND id IN ({ids})"
            ))
            .bind(Utc::now().timestamp())
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected(),
            "disable" => sqlx::query(&format!(
                "UPDATE tasks SET disabled=1,updated_at=? WHERE owner_id=? AND id IN ({ids})"
            ))
            .bind(Utc::now().timestamp())
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected(),
            "delete" => sqlx::query(&format!(
                "DELETE FROM tasks WHERE owner_id=? AND id IN ({ids})"
            ))
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected(),
            "run" => {
                // Enqueue a run for each task; count the ones that were enqueued.
                let mut count = 0_usize;
                for id in &input.ids {
                    if self.get_for_owner(*id, owner_id).await?.is_some()
                        && self.enqueue_run(*id).await?.is_some()
                    {
                        count += 1;
                    }
                }
                return Ok(count);
            }
            action => anyhow::bail!("unsupported batch action: {action}"),
        };
        Ok(usize::try_from(updated).unwrap_or(usize::MAX))
    }

    // ---- Template search / pagination ----

    pub async fn search_templates_for_owner(
        &self,
        owner_id: i64,
        query: Option<&str>,
        grp: Option<&str>,
        cursor: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Template>> {
        let limit = limit.clamp(1, 200);
        let like = query
            .map(|q| format!("%{}%", q.trim()))
            .filter(|q| !q.is_empty());
        let mut statement = format!(
            "{TEMPLATE_FIELDS} WHERE owner_id=? {}",
            match (like.is_some(), grp.is_some()) {
                (true, true) => "AND name LIKE ? AND grp=?",
                (true, false) => "AND name LIKE ?",
                (false, true) => "AND grp=?",
                (false, false) => "",
            }
        );
        if cursor.is_some() {
            statement.push_str(" AND id>?");
        }
        statement.push_str(" ORDER BY id LIMIT ?");
        let mut query_builder = sqlx::query(&statement).bind(owner_id);
        if let Some(like) = like {
            query_builder = query_builder.bind(like);
        }
        if let Some(grp) = grp {
            query_builder = query_builder.bind(grp);
        }
        if let Some(cursor) = cursor {
            query_builder = query_builder.bind(cursor);
        }
        query_builder = query_builder.bind(limit + 1);
        let rows = query_builder.fetch_all(&self.pool).await?;
        rows.into_iter().map(template_from_row).collect()
    }

    // ---- Admin: user management ----

    pub async fn list_users(&self) -> Result<Vec<User>> {
        let rows = sqlx::query(&format!("{USER_FIELDS} ORDER BY id"))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|r| user_from_row(&r)).collect()
    }

    pub async fn update_user(&self, id: i64, input: &AdminUserUpdate) -> Result<Option<User>> {
        let Some(current) = self.get_user(id).await? else {
            return Ok(None);
        };
        let role = input.role.clone().unwrap_or(current.role);
        let disabled = input.disabled.unwrap_or(current.disabled);
        ensure!(
            matches!(role.as_str(), "admin" | "user"),
            "invalid user role"
        );
        sqlx::query("UPDATE users SET role=?, disabled=?, updated_at=? WHERE id=?")
            .bind(role)
            .bind(disabled)
            .bind(Utc::now().timestamp())
            .bind(id)
            .execute(&self.pool)
            .await?;
        self.get_user(id).await
    }

    /// Delete a user and everything they own. Most foreign keys cascade, but
    /// rows are removed explicitly in dependency order (mirroring restore) so
    /// behaviour is identical on SQLite and MySQL regardless of cascade config
    /// and so `tasks.template_id ON DELETE RESTRICT` never blocks template
    /// removal. Audit logs are kept (actor_user_id is SET NULL).
    pub async fn delete_user(&self, id: i64) -> Result<bool> {
        if self.get_user(id).await?.is_none() {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM notification_actions WHERE channel_id IN (SELECT id FROM notification_channels WHERE owner_id=?) OR task_id IN (SELECT id FROM tasks WHERE owner_id=?)",
        )
        .bind(id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM notification_channels WHERE owner_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM subscription_syncs WHERE subscription_id IN (SELECT id FROM template_subscriptions WHERE owner_id=?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM template_subscriptions WHERE owner_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM push_requests WHERE owner_id=? OR reviewed_by=?")
            .bind(id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM run_steps WHERE run_id IN (SELECT r.id FROM runs r JOIN tasks t ON t.id=r.task_id WHERE t.owner_id=?)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM runs WHERE task_id IN (SELECT id FROM tasks WHERE owner_id=?)")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM tasks WHERE owner_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM templates WHERE owner_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM plugins WHERE owner_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE user_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM password_reset_tokens WHERE user_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM email_verification_tokens WHERE user_id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM users WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(deleted > 0)
    }

    // ---- Site settings ----

    pub async fn get_setting(&self, key: &str) -> Result<Option<SiteSetting>> {
        let row = sqlx::query("SELECT `key`,value,updated_at FROM site_settings WHERE `key`=?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.map(setting_from_row).transpose()
    }

    pub async fn set_setting(&self, key: &str, input: &SetSiteSetting) -> Result<SiteSetting> {
        sqlx::query(
            &format!(
                "INSERT INTO site_settings(`key`,value,updated_at) VALUES(?,?,?) {}",
                $setting_upsert
            )
        )
        .bind(key)
        .bind(serde_json::to_string(&input.value)?)
        .bind(Utc::now().timestamp())
        .execute(&self.pool)
        .await?;
        self.get_setting(key).await?.context("setting disappeared")
    }

    pub async fn list_settings(&self) -> Result<Vec<SiteSetting>> {
        let rows = sqlx::query("SELECT `key`,value,updated_at FROM site_settings ORDER BY `key`")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(setting_from_row).collect()
    }

    // ---- Password reset ----

    pub async fn create_password_reset_token(
        &self,
        user_id: i64,
        ttl_seconds: i64,
    ) -> Result<(String, i64)> {
        let token = new_token();
        let now = Utc::now().timestamp();
        let expires_at = now + ttl_seconds.max(60);
        sqlx::query(
            "INSERT INTO password_reset_tokens(token_hash,user_id,created_at,expires_at) VALUES(?,?,?,?)",
        )
        .bind(token_hash(&token))
        .bind(user_id)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok((token, expires_at))
    }

    /// Redeem a reset token; on success returns the user id and revokes the token.
    pub async fn consume_password_reset_token(
        &self,
        token: &str,
        new_password_hash: &str,
    ) -> Result<Option<i64>> {
        let now = Utc::now().timestamp();
        let row = sqlx::query(
            "SELECT user_id FROM password_reset_tokens WHERE token_hash=? AND expires_at>?",
        )
        .bind(token_hash(token))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let user_id: i64 = row.try_get("user_id")?;
        self.change_password(user_id, new_password_hash).await?;
        sqlx::query("DELETE FROM password_reset_tokens WHERE token_hash=?")
            .bind(token_hash(token))
            .execute(&self.pool)
            .await?;
        Ok(Some(user_id))
    }

    pub async fn purge_expired_reset_tokens(&self) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM password_reset_tokens WHERE expires_at<=?")
                .bind(Utc::now().timestamp())
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    // ---- Log retention ----

    /// Delete runs (and their steps via cascade) finished before the given timestamp.
    pub async fn prune_run_logs(&self, before: i64) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM runs WHERE finished_at IS NOT NULL AND finished_at<?")
                .bind(before)
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    pub async fn count_old_runs(&self, before: i64) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE finished_at IS NOT NULL AND finished_at<?",
        )
        .bind(before)
        .fetch_one(&self.pool)
        .await?)
    }

    // ---- Email verification (MustVerifyEmail) ----

    pub async fn set_user_email(&self, user_id: i64, email: &str) -> Result<bool> {
        let email = email.trim().to_lowercase();
        ensure!(email.contains('@') && email.len() <= 255, "invalid email address");
        Ok(
            sqlx::query("UPDATE users SET email=?,email_verified=0,updated_at=? WHERE id=?")
                .bind(&email)
                .bind(Utc::now().timestamp())
                .bind(user_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn create_email_verification_token(
        &self,
        user_id: i64,
        ttl_seconds: i64,
    ) -> Result<(String, i64)> {
        let token = new_token();
        let now = Utc::now().timestamp();
        let expires_at = now
            .checked_add(ttl_seconds.max(60))
            .context("verification token expiry overflow")?;
        sqlx::query("DELETE FROM email_verification_tokens WHERE user_id=?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO email_verification_tokens(token_hash,user_id,created_at,expires_at) VALUES(?,?,?,?)",
        )
        .bind(token_hash(&token))
        .bind(user_id)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok((token, expires_at))
    }

    pub async fn consume_email_verification_token(&self, token: &str) -> Result<Option<i64>> {
        let now = Utc::now().timestamp();
        let row = sqlx::query(
            "SELECT user_id FROM email_verification_tokens WHERE token_hash=? AND expires_at>?",
        )
        .bind(token_hash(token))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let user_id: i64 = row.try_get("user_id")?;
        sqlx::query("UPDATE users SET email_verified=1,updated_at=? WHERE id=?")
            .bind(now)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM email_verification_tokens WHERE token_hash=?")
            .bind(token_hash(token))
            .execute(&self.pool)
            .await?;
        Ok(Some(user_id))
    }

    pub async fn purge_expired_email_tokens(&self) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM email_verification_tokens WHERE expires_at<=?")
                .bind(Utc::now().timestamp())
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }

    // ---- CSRF rotation ----

    pub async fn rotate_csrf(&self, session_token: &str, new_csrf_hash: &str) -> Result<bool> {
        Ok(
            sqlx::query("UPDATE sessions SET csrf_token_hash=? WHERE token_hash=?")
                .bind(new_csrf_hash)
                .bind(token_hash(session_token))
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    // ---- Template subscriptions ----

    pub async fn create_subscription(
        &self,
        owner_id: i64,
        input: CreateTemplateSubscription,
    ) -> Result<TemplateSubscription> {
        let name = input.name.trim().to_string();
        let url = input.url.trim().to_string();
        ensure!(!name.is_empty() && name.len() <= 255, "invalid subscription name");
        ensure!(
            url.starts_with("https://") || url.starts_with("http://"),
            "subscription URL must be http(s)"
        );
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO template_subscriptions(owner_id,name,url,enabled,created_at,updated_at) VALUES(?,?,?,1,?,?)",
        )
        .bind(owner_id)
        .bind(&name)
        .bind(&url)
        .bind(now)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_subscription(id, owner_id)
            .await?
            .context("created subscription disappeared")
    }

    pub async fn list_subscriptions(&self, owner_id: i64) -> Result<Vec<TemplateSubscription>> {
        let rows = sqlx::query(
            "SELECT id,owner_id,name,url,enabled,last_synced_at,last_error,created_at,updated_at FROM template_subscriptions WHERE owner_id=? ORDER BY id",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(subscription_from_row).collect()
    }

    /// All enabled subscriptions across every user, used by the scheduler's
    /// periodic auto-sync loop.
    pub async fn list_enabled_subscriptions(&self) -> Result<Vec<TemplateSubscription>> {
        let rows = sqlx::query(
            "SELECT id,owner_id,name,url,enabled,last_synced_at,last_error,created_at,updated_at FROM template_subscriptions WHERE enabled=1 ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(subscription_from_row).collect()
    }

    pub async fn get_subscription(
        &self,
        id: i64,
        owner_id: i64,
    ) -> Result<Option<TemplateSubscription>> {
        let row = sqlx::query(
            "SELECT id,owner_id,name,url,enabled,last_synced_at,last_error,created_at,updated_at FROM template_subscriptions WHERE id=? AND owner_id=?",
        )
        .bind(id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(subscription_from_row).transpose()
    }

    pub async fn update_subscription(
        &self,
        id: i64,
        owner_id: i64,
        input: UpdateTemplateSubscription,
    ) -> Result<Option<TemplateSubscription>> {
        let Some(current) = self.get_subscription(id, owner_id).await? else {
            return Ok(None);
        };
        let name = input.name.unwrap_or(current.name);
        let url = input.url.unwrap_or(current.url);
        ensure!(!name.trim().is_empty() && name.len() <= 255, "invalid subscription name");
        ensure!(
            url.starts_with("https://") || url.starts_with("http://"),
            "subscription URL must be http(s)"
        );
        let enabled = input.enabled.unwrap_or(current.enabled);
        sqlx::query(
            "UPDATE template_subscriptions SET name=?,url=?,enabled=?,updated_at=? WHERE id=? AND owner_id=?",
        )
        .bind(name.trim())
        .bind(url.trim())
        .bind(enabled)
        .bind(Utc::now().timestamp())
        .bind(id)
        .bind(owner_id)
        .execute(&self.pool)
        .await?;
        self.get_subscription(id, owner_id).await
    }

    pub async fn delete_subscription(&self, id: i64, owner_id: i64) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM template_subscriptions WHERE id=? AND owner_id=?")
                .bind(id)
                .bind(owner_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    pub async fn mark_subscription_synced(
        &self,
        id: i64,
        owner_id: i64,
        error: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        sqlx::query(
            "UPDATE template_subscriptions SET last_synced_at=?,last_error=?,updated_at=? WHERE id=? AND owner_id=?",
        )
        .bind(now)
        .bind(error)
        .bind(now)
        .bind(id)
        .bind(owner_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn create_subscription_sync(
        &self,
        subscription_id: i64,
    ) -> Result<SubscriptionSync> {
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO subscription_syncs(subscription_id,status,created_at) VALUES(?,'pending',?)",
        )
        .bind(subscription_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_subscription_sync(id).await?.context("sync record disappeared")
    }

    pub async fn get_subscription_sync(&self, id: i64) -> Result<Option<SubscriptionSync>> {
        let row = sqlx::query(
            "SELECT id,subscription_id,status,message,created_at,finished_at FROM subscription_syncs WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(subscription_sync_from_row).transpose()
    }

    pub async fn finish_subscription_sync(
        &self,
        id: i64,
        status: &str,
        message: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE subscription_syncs SET status=?,message=?,finished_at=? WHERE id=?",
        )
        .bind(status)
        .bind(message)
        .bind(Utc::now().timestamp())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_subscription_syncs(
        &self,
        subscription_id: i64,
        owner_id: i64,
    ) -> Result<Option<Vec<SubscriptionSync>>> {
        if self.get_subscription(subscription_id, owner_id).await?.is_none() {
            return Ok(None);
        }
        let rows = sqlx::query(
            "SELECT id,subscription_id,status,message,created_at,finished_at FROM subscription_syncs WHERE subscription_id=? ORDER BY id DESC LIMIT 50",
        )
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(subscription_sync_from_row).collect::<Result<Vec<_>>>().map(Some)
    }

    // ---- Push requests (template publication approval) ----

    pub async fn create_push_request(
        &self,
        owner_id: i64,
        input: CreatePushRequest,
    ) -> Result<Option<PushRequest>> {
        if self.get_template_for_owner(input.template_id, owner_id).await?.is_none() {
            return Ok(None);
        }
        let already_public: bool = sqlx::query_scalar(
            "SELECT published FROM templates WHERE id=? AND owner_id=?",
        )
        .bind(input.template_id)
        .bind(owner_id)
        .fetch_one(&self.pool)
        .await?;
        if already_public {
            return Ok(None);
        }
        ensure!(
            input.note.as_deref().map(str::trim).map_or(true, |n| !n.is_empty()),
            "push request note cannot be empty"
        );
        let now = Utc::now().timestamp();
        let mut conn = self.pool.acquire().await?;
        sqlx::query(
            "INSERT INTO push_requests(owner_id,template_id,status,note,created_at) VALUES(?,?,'pending',?,?)",
        )
        .bind(owner_id)
        .bind(input.template_id)
        .bind(input.note.as_deref().map(str::trim))
        .bind(now)
        .execute(&mut *conn)
        .await?;
        let id = self.last_insert_id(&mut conn).await?;
        drop(conn);
        self.get_push_request(id).await?.context("push request disappeared").map(Some)
    }

    pub async fn get_push_request(&self, id: i64) -> Result<Option<PushRequest>> {
        let row = sqlx::query(
            "SELECT id,owner_id,template_id,status,note,reviewed_by,reviewed_at,created_at FROM push_requests WHERE id=?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(push_request_from_row).transpose()
    }

    pub async fn list_push_requests_for_owner(&self, owner_id: i64) -> Result<Vec<PushRequest>> {
        let rows = sqlx::query(
            "SELECT id,owner_id,template_id,status,note,reviewed_by,reviewed_at,created_at FROM push_requests WHERE owner_id=? ORDER BY created_at DESC,id DESC",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(push_request_from_row).collect()
    }

    pub async fn list_push_requests(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<PushRequest>> {
        let rows = match status {
            Some(status) => {
                sqlx::query(
                    "SELECT id,owner_id,template_id,status,note,reviewed_by,reviewed_at,created_at FROM push_requests WHERE status=? ORDER BY created_at DESC,id DESC",
                )
                .bind(status)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id,owner_id,template_id,status,note,reviewed_by,reviewed_at,created_at FROM push_requests ORDER BY created_at DESC,id DESC",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.into_iter().map(push_request_from_row).collect()
    }

    pub async fn decide_push_request(
        &self,
        id: i64,
        reviewer_id: i64,
        input: &DecidePushRequest,
    ) -> Result<Option<PushRequest>> {
        let Some(request) = self.get_push_request(id).await? else {
            return Ok(None);
        };
        if request.status != "pending" {
            return Ok(None);
        }
        let now = Utc::now().timestamp();
        let status = if input.approve { "approved" } else { "rejected" };
        sqlx::query(
            "UPDATE push_requests SET status=?,note=COALESCE(?,note),reviewed_by=?,reviewed_at=? WHERE id=?",
        )
        .bind(status)
        .bind(input.note.as_deref())
        .bind(reviewer_id)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if input.approve {
            let _ = self
                .set_template_published(request.template_id, request.owner_id, true)
                .await;
        }
        self.get_push_request(id).await
    }

    /// Find an existing QD HAR template by owner and display name so a
    /// subscription re-sync can update it in place instead of duplicating it.
    pub async fn find_template_by_name(
        &self,
        owner_id: i64,
        name: &str,
    ) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM templates WHERE owner_id=? AND name=? AND source_format='qd_har' ORDER BY id LIMIT 1",
        )
        .bind(owner_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?)
    }

    // ---- Backup & restore ----

    pub async fn export_data(&self) -> Result<serde_json::Value> {
        use serde_json::{Map, Value as JsonValue};
        let mut out = Map::new();
        // Metadata so restores can be validated before the destructive import.
        out.insert("schema_version".to_string(), JsonValue::from(1));
        out.insert(
            "exported_at".to_string(),
            JsonValue::from(Utc::now().timestamp()),
        );
        let tables = [
            "users",
            "sessions",
            "templates",
            "tasks",
            "runs",
            "run_steps",
            "notification_channels",
            "notification_actions",
            "plugins",
            "site_settings",
            "template_subscriptions",
            "subscription_syncs",
            "push_requests",
        ];
        for table in tables {
            let rows: Vec<JsonValue> = sqlx::query(&format!("SELECT * FROM {table}"))
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .map(|row| {
                    let mut m = Map::new();
                    for column in row.columns() {
                        let name = column.name().to_string();
                        let value =
                            if let Ok(v) = row.try_get::<Option<i64>, _>(name.as_str()) {
                                v.map(JsonValue::from).unwrap_or(JsonValue::Null)
                            } else if let Ok(v) = row.try_get::<Option<f64>, _>(name.as_str()) {
                                v.map(JsonValue::from).unwrap_or(JsonValue::Null)
                            } else if let Ok(v) = row.try_get::<Option<String>, _>(name.as_str()) {
                                v.map(JsonValue::String).unwrap_or(JsonValue::Null)
                            } else if let Ok(v) = row.try_get::<Option<Vec<u8>>, _>(name.as_str()) {
                                v.map(|b| JsonValue::String(String::from_utf8_lossy(&b).into_owned()))
                                    .unwrap_or(JsonValue::Null)
                            } else {
                                JsonValue::Null
                            };
                        m.insert(name, value);
                    }
                    Ok::<_, anyhow::Error>(JsonValue::Object(m))
                })
                .collect::<Result<Vec<_>>>()?;
            out.insert(table.to_string(), JsonValue::Array(rows));
        }
        Ok(JsonValue::Object(out))
    }

    pub async fn import_data(&self, backup: &serde_json::Value) -> Result<()> {
        use serde_json::Value as JsonValue;
        let Some(object) = backup.as_object() else {
            anyhow::bail!("backup must be a JSON object of tables");
        };
        // Destructive full-table import: refuse anything that was not produced
        // by a matching exporter so a wrong payload cannot wipe the database.
        let version = object
            .get("schema_version")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        ensure!(
            version == 1,
            "unsupported backup schema_version {version} (expected 1)"
        );
        let mut tx = self.pool.begin().await?;
        let tables = [
            "push_requests",
            "subscription_syncs",
            "template_subscriptions",
            "site_settings",
            "plugins",
            "notification_actions",
            "notification_channels",
            "run_steps",
            "runs",
            "tasks",
            "templates",
            "sessions",
            "users",
        ];
        for table in tables {
            sqlx::query(&format!("DELETE FROM {table}"))
                .execute(&mut *tx)
                .await?;
        }
        // Import in dependency order regardless of the JSON key order.
        let import_order = [
            "users",
            "sessions",
            "templates",
            "tasks",
            "runs",
            "run_steps",
            "notification_channels",
            "notification_actions",
            "plugins",
            "site_settings",
            "template_subscriptions",
            "subscription_syncs",
            "push_requests",
        ];
        for table in import_order {
            let Some(rows) = object.get(table).and_then(|v| v.as_array()) else {
                continue;
            };
            if rows.is_empty() {
                continue;
            }
            let columns: Vec<String> = rows[0]
                .as_object()
                .map(|row| row.keys().cloned().collect())
                .unwrap_or_default();
            if columns.is_empty() {
                continue;
            }
            let column_list = columns.join(",");
            let placeholders = columns.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            for row in rows {
                let Some(row_map) = row.as_object() else {
                    continue;
                };
                let sql = format!(
                    "INSERT INTO {table}({column_list}) VALUES({placeholders})"
                );
                let mut query = sqlx::query::<$db>(&sql);
                for column in &columns {
                    let value = row_map.get(column).cloned().unwrap_or(JsonValue::Null);
                    query = match value {
                        JsonValue::Null => query.bind(Option::<i64>::None),
                        JsonValue::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                query.bind(i)
                            } else if let Some(f) = n.as_f64() {
                                query.bind(f)
                            } else {
                                query.bind(Option::<i64>::None)
                            }
                        }
                        JsonValue::String(s) => query.bind(s),
                        JsonValue::Bool(b) => query.bind(b),
                        other => query.bind(other.to_string()),
                    };
                }
                query.execute(&mut *tx).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
            }

fn setting_from_row(row: $row) -> Result<SiteSetting> {
    Ok(SiteSetting {
        key: row.try_get("key")?,
        value: serde_json::from_str(&row.try_get::<String, _>("value")?)?,
        updated_at: row.try_get("updated_at")?,
    })
}

const TASK_FIELDS: &str = "SELECT id,name,cron,method,url,headers,body,disabled,created_at,updated_at,last_run_at,last_status,last_error,template_id,grp,timeout_seconds,retry_count,retry_interval_seconds,priority,timezone,random_delay_max_seconds,variables FROM tasks";
const TEMPLATE_FIELDS: &str = "SELECT id,name,description,schema_version,definition,source_format,source,created_at,updated_at,grp FROM templates";
const RUN_FIELDS: &str = "SELECT id,task_id,status,http_status,error,log,started_at,finished_at,created_at,lease_owner,lease_expires_at,attempt,cancel_requested,run_after,retry_of FROM runs";
const USER_FIELDS: &str = "SELECT id,username,role,disabled,email,email_verified,created_at,updated_at FROM users";

fn user_from_row(row: &$row) -> Result<User> {
    Ok(User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        role: row.try_get("role")?,
        disabled: row.try_get("disabled")?,
        email: row.try_get("email")?,
        email_verified: row.try_get("email_verified")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn credentials_from_row(row: &$row) -> Result<UserCredentials> {
    Ok(UserCredentials {
        user: user_from_row(row)?,
        password_hash: row.try_get("password_hash")?,
        session_version: row.try_get("session_version")?,
    })
}

fn authenticated_session_from_row(row: &$row) -> Result<AuthenticatedSession> {
    Ok(AuthenticatedSession {
        user: user_from_row(row)?,
        csrf_token_hash: row.try_get("csrf_token_hash")?,
        expires_at: row.try_get("expires_at")?,
    })
}

fn task_from_row(row: $row) -> Result<Task> {
    let headers: String = row.try_get("headers")?;
    let variables: Option<String> = row.try_get("variables")?;
    Ok(Task {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        cron: row.try_get("cron")?,
        method: row.try_get("method")?,
        url: row.try_get("url")?,
        headers: serde_json::from_str(&headers).context("invalid task headers in database")?,
        body: row.try_get("body")?,
        disabled: row.try_get("disabled")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        last_run_at: row.try_get("last_run_at")?,
        last_status: row.try_get("last_status")?,
        last_error: row.try_get("last_error")?,
        template_id: row.try_get("template_id")?,
        grp: row.try_get("grp")?,
        timeout_seconds: row.try_get("timeout_seconds")?,
        retry_count: row.try_get("retry_count")?,
        retry_interval_seconds: row.try_get("retry_interval_seconds")?,
        priority: row.try_get("priority")?,
        timezone: row.try_get("timezone")?,
        random_delay_max_seconds: row.try_get("random_delay_max_seconds")?,
        variables: variables
            .map(|v| serde_json::from_str(&v).context("invalid task variables in database"))
            .transpose()?,
    })
}

fn run_step_from_row(row: $row) -> Result<RunStep> {
    Ok(RunStep {
        id: row.try_get("id")?,
        run_id: row.try_get("run_id")?,
        step_index: row.try_get("step_index")?,
        name: row.try_get("name")?,
        status: row.try_get("status")?,
        http_status: row.try_get("http_status")?,
        body_size: row.try_get("body_size")?,
        error: row.try_get("error")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
    })
}

fn template_from_row(row: $row) -> Result<Template> {
    let definition: String = row.try_get("definition")?;
    let source_format: String = row.try_get("source_format")?;
    let source: Option<String> = row.try_get("source")?;
    let (definition, qd_har) = match source_format.as_str() {
        "qd_har" => (
            None,
            Some(serde_json::from_str(
                source.as_deref().context("QD HAR source is missing")?,
            )?),
        ),
        "native_v1" => (
            Some(
                serde_json::from_str(&definition)
                    .context("invalid template definition in database")?,
            ),
            None,
        ),
        value => return Err(anyhow!("unsupported template source format: {value}")),
    };
    Ok(Template {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        schema_version: row.try_get("schema_version")?,
        source_format,
        definition,
        qd_har,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        grp: row.try_get("grp")?,
    })
}

fn run_from_row(row: $row) -> Result<Run> {
    Ok(Run {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        status: row.try_get("status")?,
        http_status: row.try_get("http_status")?,
        error: row.try_get("error")?,
        log: row.try_get("log")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        created_at: row.try_get("created_at")?,
        lease_owner: row.try_get("lease_owner")?,
        lease_expires_at: row.try_get("lease_expires_at")?,
        attempt: row.try_get("attempt")?,
        cancel_requested: row.try_get("cancel_requested")?,
        run_after: row.try_get("run_after")?,
        retry_of: row.try_get("retry_of")?,
    })
}

fn notification_from_row(row: $row) -> Result<NotificationChannel> {
    Ok(NotificationChannel {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        kind: row.try_get("kind")?,
        config: serde_json::from_str(&row.try_get::<String, _>("config")?)?,
        enabled: row.try_get("enabled")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn notification_action_from_row(row: $row) -> Result<NotificationAction> {
    Ok(NotificationAction {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        channel_id: row.try_get("channel_id")?,
        event: row.try_get("event")?,
        created_at: row.try_get("created_at")?,
    })
}

fn plugin_from_row(row: $row) -> Result<PluginManifest> {
    Ok(PluginManifest {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        command: row.try_get("command")?,
        config: serde_json::from_str(&row.try_get::<String, _>("config")?)?,
        enabled: row.try_get("enabled")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn subscription_from_row(row: $row) -> Result<TemplateSubscription> {
    Ok(TemplateSubscription {
        id: row.try_get("id")?,
        owner_id: row.try_get("owner_id")?,
        name: row.try_get("name")?,
        url: row.try_get("url")?,
        enabled: row.try_get("enabled")?,
        last_synced_at: row.try_get("last_synced_at")?,
        last_error: row.try_get("last_error")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn subscription_sync_from_row(row: $row) -> Result<SubscriptionSync> {
    Ok(SubscriptionSync {
        id: row.try_get("id")?,
        subscription_id: row.try_get("subscription_id")?,
        status: row.try_get("status")?,
        message: row.try_get("message")?,
        created_at: row.try_get("created_at")?,
        finished_at: row.try_get("finished_at")?,
    })
}

fn push_request_from_row(row: $row) -> Result<PushRequest> {
    Ok(PushRequest {
        id: row.try_get("id")?,
        owner_id: row.try_get("owner_id")?,
        template_id: row.try_get("template_id")?,
        status: row.try_get("status")?,
        note: row.try_get("note")?,
        reviewed_by: row.try_get("reviewed_by")?,
        reviewed_at: row.try_get("reviewed_at")?,
        created_at: row.try_get("created_at")?,
    })
}
            }
        }
    }
define_store!(
    sqlite_store,
    Sqlite,
    SqlitePool,
    SqlitePoolOptions,
    sqlite_options,
    &SQLITE_MIGRATOR,
    SqliteRow,
    "SELECT last_insert_rowid()",
    sqlite_username_cmp,
    "ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
    sqlite_parent_check
);

define_store!(
    mysql_store,
    MySql,
    MySqlPool,
    MySqlPoolOptions,
    mysql_options,
    &MYSQL_MIGRATOR,
    MySqlRow,
    "SELECT CAST(LAST_INSERT_ID() AS SIGNED)",
    mysql_username_cmp,
    "ON DUPLICATE KEY UPDATE value=VALUES(value), updated_at=VALUES(updated_at)",
    no_parent_check
);
macro_rules! delegate {
    ($(pub async fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty { $($call:ident),* };)*) => {
        $(
            pub async fn $name(&self, $($arg: $ty),*) -> $ret {
                match self {
                    Store::Sqlite(s) => s.$name($($call),*).await,
                    Store::MySql(s) => s.$name($($call),*).await,
                }
            }
        )*
    };
}

impl Store {
    #[cfg(test)]
    pub(crate) fn sqlite_pool(&self) -> &SqlitePool {
        match self {
            Store::Sqlite(s) => &s.pool,
            Store::MySql(_) => panic!("mysql not available in tests"),
        }
    }
}

/// Runtime database backend. The API and scheduler layers work with this enum
/// so SQLite and MySQL deployments share one binary and one code path.
#[derive(Clone)]
pub enum Store {
    Sqlite(sqlite_store::Store),
    MySql(mysql_store::Store),
}

impl Store {
    pub async fn connect(url: &str, min_connections: u32, max_connections: u32) -> Result<Self> {
        Self::connect_with_timeouts(
            url,
            min_connections,
            max_connections,
            Duration::from_secs(30),
            Duration::from_secs(600),
        )
        .await
    }

    pub async fn connect_with_timeouts(
        url: &str,
        min_connections: u32,
        max_connections: u32,
        acquire_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<Self> {
        if url.starts_with("mysql://") || url.starts_with("mysql+") {
            Ok(Self::MySql(
                mysql_store::Store::connect_with_timeouts(
                    url,
                    min_connections,
                    max_connections,
                    acquire_timeout,
                    idle_timeout,
                )
                .await?,
            ))
        } else {
            Ok(Self::Sqlite(
                sqlite_store::Store::connect_with_timeouts(
                    url,
                    min_connections,
                    max_connections,
                    acquire_timeout,
                    idle_timeout,
                )
                .await?,
            ))
        }
    }

    delegate! {
        pub async fn ready() -> Result<()> {  };
        pub async fn create_user(username: &str, password_hash: &str, role: &str) -> Result<User> { username, password_hash, role };
        pub async fn create_first_admin(username: &str, password_hash: &str) -> Result<Option<User>> { username, password_hash };
        pub async fn get_user(id: i64) -> Result<Option<User>> { id };
        pub async fn credentials_by_username(username: &str) -> Result<Option<UserCredentials>> { username };
        pub async fn create_session(user_id: i64, ttl: Duration) -> Result<IssuedSession> { user_id, ttl };
        pub async fn authenticate_session(session_token: &str) -> Result<Option<AuthenticatedSession>> { session_token };
        pub async fn revoke_session(session_token: &str) -> Result<bool> { session_token };
        pub async fn revoke_all_sessions(user_id: i64) -> Result<()> { user_id };
        pub async fn change_password(user_id: i64, password_hash: &str) -> Result<bool> { user_id, password_hash };
        pub async fn record_audit(actor_user_id: Option<i64>, action: &str, resource_type: Option<&str>, resource_id: Option<i64>, request_id: Option<&str>, details: &serde_json::Value) -> Result<()> { actor_user_id, action, resource_type, resource_id, request_id, details };
        pub async fn purge_expired_sessions() -> Result<u64> {  };
        pub async fn create(input: CreateTask) -> Result<Task> { input };
        pub async fn create_for_owner(owner_id: i64, input: CreateTask) -> Result<Task> { owner_id, input };
        pub async fn list() -> Result<Vec<Task>> {  };
        pub async fn list_for_owner(owner_id: i64) -> Result<Vec<Task>> { owner_id };
        pub async fn list_for_owner_with_group(owner_id: i64, grp: Option<&str>) -> Result<Vec<Task>> { owner_id, grp };
        pub async fn get(id: i64) -> Result<Option<Task>> { id };
        pub async fn get_for_owner(id: i64, owner_id: i64) -> Result<Option<Task>> { id, owner_id };
        pub async fn update(id: i64, input: UpdateTask) -> Result<Option<Task>> { id, input };
        pub async fn update_for_owner(id: i64, owner_id: i64, input: UpdateTask) -> Result<Option<Task>> { id, owner_id, input };
        pub async fn delete(id: i64) -> Result<bool> { id };
        pub async fn delete_for_owner(id: i64, owner_id: i64) -> Result<bool> { id, owner_id };
        pub async fn record_run(id: i64, status: Option<u16>, error: Option<&str>) -> Result<()> { id, status, error };
        pub async fn start_run(task_id: i64) -> Result<Run> { task_id };
        pub async fn enqueue_run(task_id: i64) -> Result<Option<Run>> { task_id };
        pub async fn enqueue_delayed_run(task_id: i64, delay_seconds: i64) -> Result<Option<Run>> { task_id, delay_seconds };
        pub async fn schedule_retry(task_id: i64, retry_of: i64, delay_seconds: i64) -> Result<Option<Run>> { task_id, retry_of, delay_seconds };
        pub async fn count_retries(original_run_id: i64) -> Result<i64> { original_run_id };
        pub async fn claim_run(worker: &str, lease_seconds: i64) -> Result<Option<Run>> { worker, lease_seconds };
        pub async fn start_leased_run(run_id: i64, worker: &str) -> Result<bool> { run_id, worker };
        pub async fn renew_run(run_id: i64, worker: &str, lease_seconds: i64) -> Result<bool> { run_id, worker, lease_seconds };
        pub async fn cancel_run(run_id: i64) -> Result<bool> { run_id };
        pub async fn recover_expired_runs() -> Result<u64> {  };
        pub async fn finish_run(run_id: i64, http_status: Option<u16>, error: Option<&str>) -> Result<()> { run_id, http_status, error };
        pub async fn record_run_log(run_id: i64, log: &str) -> Result<()> { run_id, log };
        pub async fn delete_run(run_id: i64) -> Result<bool> { run_id };
        pub async fn delete_runs_for_owner(owner_id: i64) -> Result<u64> { owner_id };
        pub async fn delete_all_runs() -> Result<u64> {  };
        pub async fn record_run_step(step: &RunStep) -> Result<()> { step };
        pub async fn list_run_steps(run_id: i64) -> Result<Vec<RunStep>> { run_id };
        pub async fn list_run_steps_for_owner(run_id: i64, owner_id: i64) -> Result<Option<Vec<RunStep>>> { run_id, owner_id };
        pub async fn get_run(id: i64) -> Result<Option<Run>> { id };
        pub async fn list_task_runs(task_id: i64) -> Result<Vec<Run>> { task_id };
        pub async fn list_task_runs_for_owner(task_id: i64, owner_id: i64) -> Result<Option<Vec<Run>>> { task_id, owner_id };
        pub async fn create_notification_channel(owner_id: i64, input: CreateNotificationChannel) -> Result<NotificationChannel> { owner_id, input };
        pub async fn list_notification_channels(owner_id: i64) -> Result<Vec<NotificationChannel>> { owner_id };
        pub async fn get_notification_channel(id: i64, owner_id: i64) -> Result<Option<NotificationChannel>> { id, owner_id };
        pub async fn update_notification_channel(id: i64, owner_id: i64, input: UpdateNotificationChannel) -> Result<Option<NotificationChannel>> { id, owner_id, input };
        pub async fn delete_notification_channel(id: i64, owner_id: i64) -> Result<bool> { id, owner_id };
        pub async fn create_notification_action(task_id: i64, owner_id: i64, input: CreateNotificationAction) -> Result<Option<NotificationAction>> { task_id, owner_id, input };
        pub async fn list_notification_actions(task_id: i64, owner_id: i64) -> Result<Option<Vec<NotificationAction>>> { task_id, owner_id };
        pub async fn delete_notification_action(id: i64, owner_id: i64) -> Result<bool> { id, owner_id };
        pub async fn notification_channels_for_event(task_id: i64, event: &str) -> Result<Vec<NotificationChannel>> { task_id, event };
        pub async fn create_plugin(owner_id: i64, input: CreatePluginManifest) -> Result<PluginManifest> { owner_id, input };
        pub async fn list_plugins(owner_id: i64) -> Result<Vec<PluginManifest>> { owner_id };
        pub async fn get_plugin(id: i64, owner_id: i64) -> Result<Option<PluginManifest>> { id, owner_id };
        pub async fn update_plugin(id: i64, owner_id: i64, input: UpdatePluginManifest) -> Result<Option<PluginManifest>> { id, owner_id, input };
        pub async fn delete_plugin(id: i64, owner_id: i64) -> Result<bool> { id, owner_id };
        pub async fn create_template(input: CreateTemplate) -> Result<Template> { input };
        pub async fn create_template_for_owner(owner_id: i64, input: CreateTemplate) -> Result<Template> { owner_id, input };
        pub async fn import_qd_har(input: ImportQdHarTemplate) -> Result<Template> { input };
        pub async fn import_qd_har_for_owner(owner_id: i64, input: ImportQdHarTemplate) -> Result<Template> { owner_id, input };
        pub async fn update_qd_har_for_owner(id: i64, owner_id: i64, input: UpdateQdHarTemplate) -> Result<Option<Template>> { id, owner_id, input };
        pub async fn list_templates() -> Result<Vec<Template>> {  };
        pub async fn list_templates_for_owner(owner_id: i64) -> Result<Vec<Template>> { owner_id };
        pub async fn get_template(id: i64) -> Result<Option<Template>> { id };
        pub async fn get_template_for_owner(id: i64, owner_id: i64) -> Result<Option<Template>> { id, owner_id };
        pub async fn update_template(id: i64, input: UpdateTemplate) -> Result<Option<Template>> { id, input };
        pub async fn update_template_for_owner(id: i64, owner_id: i64, input: UpdateTemplate) -> Result<Option<Template>> { id, owner_id, input };
        pub async fn delete_template(id: i64) -> Result<bool> { id };
        pub async fn delete_template_for_owner(id: i64, owner_id: i64) -> Result<bool> { id, owner_id };
        pub async fn set_template_published(id: i64, owner_id: i64, published: bool) -> Result<bool> { id, owner_id, published };
        pub async fn list_public_templates() -> Result<Vec<Template>> {  };
        pub async fn copy_public_template(id: i64, owner_id: i64) -> Result<Option<Template>> { id, owner_id };
        pub async fn list_groups_for_owner(owner_id: i64) -> Result<Vec<String>> { owner_id };
        pub async fn batch_operations_for_owner(owner_id: i64, input: &BatchTaskOperation) -> Result<usize> { owner_id, input };
        pub async fn search_templates_for_owner(owner_id: i64, query: Option<&str>, grp: Option<&str>, cursor: Option<i64>, limit: i64) -> Result<Vec<Template>> { owner_id, query, grp, cursor, limit };
        pub async fn list_users() -> Result<Vec<User>> {  };
        pub async fn update_user(id: i64, input: &AdminUserUpdate) -> Result<Option<User>> { id, input };
        pub async fn delete_user(id: i64) -> Result<bool> { id };
        pub async fn get_setting(key: &str) -> Result<Option<SiteSetting>> { key };
        pub async fn set_setting(key: &str, input: &SetSiteSetting) -> Result<SiteSetting> { key, input };
        pub async fn list_settings() -> Result<Vec<SiteSetting>> {  };
        pub async fn create_password_reset_token(user_id: i64, ttl_seconds: i64) -> Result<(String, i64)> { user_id, ttl_seconds };
        pub async fn consume_password_reset_token(token: &str, new_password_hash: &str) -> Result<Option<i64>> { token, new_password_hash };
        pub async fn purge_expired_reset_tokens() -> Result<u64> {  };
        pub async fn prune_run_logs(before: i64) -> Result<u64> { before };
        pub async fn count_old_runs(before: i64) -> Result<i64> { before };
        pub async fn set_user_email(user_id: i64, email: &str) -> Result<bool> { user_id, email };
        pub async fn create_email_verification_token(user_id: i64, ttl_seconds: i64) -> Result<(String, i64)> { user_id, ttl_seconds };
        pub async fn consume_email_verification_token(token: &str) -> Result<Option<i64>> { token };
        pub async fn purge_expired_email_tokens() -> Result<u64> {  };
        pub async fn rotate_csrf(session_token: &str, new_csrf_hash: &str) -> Result<bool> { session_token, new_csrf_hash };
        pub async fn create_subscription(owner_id: i64, input: CreateTemplateSubscription) -> Result<TemplateSubscription> { owner_id, input };
        pub async fn list_subscriptions(owner_id: i64) -> Result<Vec<TemplateSubscription>> { owner_id };
        pub async fn list_enabled_subscriptions() -> Result<Vec<TemplateSubscription>> {  };
        pub async fn get_subscription(id: i64, owner_id: i64) -> Result<Option<TemplateSubscription>> { id, owner_id };
        pub async fn update_subscription(id: i64, owner_id: i64, input: UpdateTemplateSubscription) -> Result<Option<TemplateSubscription>> { id, owner_id, input };
        pub async fn delete_subscription(id: i64, owner_id: i64) -> Result<bool> { id, owner_id };
        pub async fn mark_subscription_synced(id: i64, owner_id: i64, error: Option<&str>) -> Result<()> { id, owner_id, error };
        pub async fn create_subscription_sync(subscription_id: i64) -> Result<SubscriptionSync> { subscription_id };
        pub async fn get_subscription_sync(id: i64) -> Result<Option<SubscriptionSync>> { id };
        pub async fn finish_subscription_sync(id: i64, status: &str, message: Option<&str>) -> Result<()> { id, status, message };
        pub async fn list_subscription_syncs(subscription_id: i64, owner_id: i64) -> Result<Option<Vec<SubscriptionSync>>> { subscription_id, owner_id };
        pub async fn create_push_request(owner_id: i64, input: CreatePushRequest) -> Result<Option<PushRequest>> { owner_id, input };
        pub async fn get_push_request(id: i64) -> Result<Option<PushRequest>> { id };
        pub async fn list_push_requests_for_owner(owner_id: i64) -> Result<Vec<PushRequest>> { owner_id };
        pub async fn list_push_requests(status: Option<&str>) -> Result<Vec<PushRequest>> { status };
        pub async fn decide_push_request(id: i64, reviewer_id: i64, input: &DecidePushRequest) -> Result<Option<PushRequest>> { id, reviewer_id, input };
        pub async fn find_template_by_name(owner_id: i64, name: &str) -> Result<Option<i64>> { owner_id, name };
        pub async fn export_data() -> Result<serde_json::Value> {  };
        pub async fn import_data(backup: &serde_json::Value) -> Result<()> { backup };
    }
}

fn validate_plugin(name: &str, command: &str, config: &serde_json::Value) -> Result<()> {
    ensure!(!name.trim().is_empty(), "plugin name cannot be empty");
    ensure!(!command.trim().is_empty(), "plugin command cannot be empty");
    ensure!(
        !command
            .chars()
            .any(|c| matches!(c, '|' | '&' | ';' | '>' | '<' | '\n' | '\r')),
        "plugin command must not contain shell operators"
    );
    ensure!(config.is_object(), "plugin config must be an object");
    Ok(())
}

fn validate_notification(name: &str, kind: &str, config: &serde_json::Value) -> Result<()> {
    ensure!(
        !name.trim().is_empty(),
        "notification channel name cannot be empty"
    );
    ensure!(config.is_object(), "notification config must be an object");
    match kind {
        "webhook" => {
            let url = config
                .get("url")
                .and_then(|v| v.as_str())
                .context("webhook config requires url")?;
            let url = reqwest::Url::parse(url).context("invalid webhook URL")?;
            ensure!(url.scheme() == "https", "webhook URL must use HTTPS");
        }
        "email" => ensure!(
            config.get("to").and_then(|v| v.as_str()).is_some(),
            "email config requires to"
        ),
        _ => anyhow::bail!("unsupported notification channel kind"),
    }
    Ok(())
}

/// Merge an optional update (`Some(v)` = set, `None` = leave unchanged) into
/// the current value. `v` itself may be `None` to clear the field.
fn merge_optional<T>(input: Option<Option<T>>, current: Option<T>) -> Option<T> {
    input.unwrap_or(current)
}

fn validate(name: &str, schedule: &str, url: &str, timezone: Option<&str>) -> Result<()> {
    if name.trim().is_empty() {
        return Err(anyhow!("name cannot be empty"));
    }
    schedule
        .parse::<cron::Schedule>()
        .context("invalid cron expression")?;
    reqwest::Url::parse(url).context("invalid task URL")?;
    if let Some(timezone) = timezone.filter(|tz| !tz.trim().is_empty()) {
        timezone
            .parse::<chrono_tz::Tz>()
            .context("invalid timezone")?;
    }
    Ok(())
}

fn validate_template_name(name: &str) -> Result<()> {
    ensure!(!name.trim().is_empty(), "template name cannot be empty");
    ensure!(name.chars().count() <= 100, "template name is too long");
    Ok(())
}

fn validate_username(username: &str) -> Result<()> {
    let username = username.trim();
    ensure!(username.chars().count() >= 3, "username is too short");
    ensure!(username.chars().count() <= 64, "username is too long");
    ensure!(
        username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')),
        "username contains unsupported characters"
    );
    Ok(())
}

fn sqlite_parent_check(url: &str) -> Result<()> {
    let Some(path) = url.strip_prefix("sqlite://") else {
        return Ok(());
    };
    if path == ":memory:" {
        return Ok(());
    }
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::auth::{hash_password, token_hash};
    use qdrust_core::template::{RequestStep, Step, TemplateDefinition};

    fn input(name: &str) -> CreateTask {
        CreateTask {
            name: name.into(),
            cron: "0 * * * * * *".into(),
            method: Some("GET".into()),
            url: "https://example.com/health".into(),
            headers: serde_json::Map::new(),
            body: None,
            disabled: false,
            template_id: None,
            grp: None,
            timeout_seconds: None,
            retry_count: None,
            retry_interval_seconds: None,
            priority: None,
            timezone: None,
            random_delay_max_seconds: None,
            variables: None,
        }
    }

    fn template_definition(name: &str) -> TemplateDefinition {
        TemplateDefinition {
            version: TEMPLATE_SCHEMA_VERSION,
            name: name.into(),
            variables: BTreeMap::new(),
            steps: vec![Step::Request(RequestStep {
                name: "request".into(),
                method: "GET".into(),
                url: "https://example.invalid/health".into(),
                headers: BTreeMap::new(),
                query: BTreeMap::new(),
                body: None,
            })],
        }
    }

    #[tokio::test]
    async fn migrates_and_runs_task_crud() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        store.ready().await.unwrap();

        let created = store.create(input("first")).await.unwrap();
        assert_eq!(created.name, "first");
        assert_eq!(store.list().await.unwrap().len(), 1);

        let updated = store
            .update(
                created.id,
                UpdateTask {
                    name: Some("renamed".into()),
                    cron: None,
                    method: None,
                    url: None,
                    headers: None,
                    body: None,
                    disabled: Some(true),
                    template_id: None,
                    grp: None,
                    timeout_seconds: None,
                    retry_count: None,
                    retry_interval_seconds: None,
                    priority: None,
                    timezone: None,
                    random_delay_max_seconds: Some(Some(120)),
                    variables: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "renamed");
        assert!(updated.disabled);
        assert_eq!(updated.random_delay_max_seconds, Some(120));
        assert!(store.delete(created.id).await.unwrap());
        assert!(store.get(created.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn creates_users_and_enforces_normalized_unique_names() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let password_hash = hash_password("correct horse battery staple").unwrap();
        let user = store
            .create_user("alice_1", &password_hash, "admin")
            .await
            .unwrap();
        assert_eq!(user.username, "alice_1");
        assert_eq!(user.role, "admin");

        let credentials = store
            .credentials_by_username("ALICE_1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.user.id, user.id);
        assert_eq!(credentials.password_hash, password_hash);
        assert!(
            store
                .create_user("Alice_1", &password_hash, "user")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn owner_scoped_resources_cannot_cross_user_boundaries() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let password_hash = hash_password("correct horse battery staple").unwrap();
        let alice = store
            .create_user("owner_alice", &password_hash, "user")
            .await
            .unwrap();
        let bob = store
            .create_user("owner_bob", &password_hash, "user")
            .await
            .unwrap();

        let task = store
            .create_for_owner(alice.id, input("alice task"))
            .await
            .unwrap();
        assert_eq!(store.list_for_owner(alice.id).await.unwrap().len(), 1);
        assert!(
            store
                .get_for_owner(task.id, bob.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .update_for_owner(
                    task.id,
                    bob.id,
                    UpdateTask {
                        name: Some("hijacked".into()),
                        cron: None,
                        method: None,
                        url: None,
                        headers: None,
                        body: None,
                        disabled: None,
                        template_id: None,
                        grp: None,
                        timeout_seconds: None,
                        retry_count: None,
                        retry_interval_seconds: None,
                        priority: None,
                        timezone: None,
                        random_delay_max_seconds: None,
                        variables: None,
                    },
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(!store.delete_for_owner(task.id, bob.id).await.unwrap());
        let run = store.start_run(task.id).await.unwrap();
        assert!(
            store
                .list_task_runs_for_owner(task.id, bob.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .list_task_runs_for_owner(task.id, alice.id)
                .await
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        store
            .finish_run(run.id, None, Some("test cleanup"))
            .await
            .unwrap();

        let template = store
            .create_template_for_owner(
                alice.id,
                CreateTemplate {
                    name: "alice template".into(),
                    description: None,
                    definition: template_definition("alice template"),
                    grp: None,
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .get_template_for_owner(template.id, bob.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !store
                .delete_template_for_owner(template.id, bob.id)
                .await
                .unwrap()
        );
        assert!(
            store
                .get_template_for_owner(template.id, alice.id)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn creates_authenticates_and_revokes_sessions() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let password_hash = hash_password("correct horse battery staple").unwrap();
        let user = store
            .create_user("session_user", &password_hash, "user")
            .await
            .unwrap();
        let first = store
            .create_session(user.id, Duration::from_secs(3600))
            .await
            .unwrap();
        let second = store
            .create_session(user.id, Duration::from_secs(3600))
            .await
            .unwrap();

        let authenticated = store
            .authenticate_session(&first.session_token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(authenticated.user.id, user.id);
        assert_eq!(authenticated.csrf_token_hash, token_hash(&first.csrf_token));
        let stored: String =
            sqlx::query_scalar("SELECT token_hash FROM sessions WHERE token_hash=?")
                .bind(token_hash(&first.session_token))
                .fetch_one(store.sqlite_pool())
                .await
                .unwrap();
        assert_ne!(stored, first.session_token);

        assert!(store.revoke_session(&first.session_token).await.unwrap());
        assert!(
            store
                .authenticate_session(&first.session_token)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .authenticate_session(&second.session_token)
                .await
                .unwrap()
                .is_some()
        );

        store.revoke_all_sessions(user.id).await.unwrap();
        assert!(
            store
                .authenticate_session(&second.session_token)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn changing_password_revokes_sessions_and_records_audit() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let old_hash = hash_password("correct horse battery staple").unwrap();
        let user = store
            .create_user("password_user", &old_hash, "user")
            .await
            .unwrap();
        let session = store
            .create_session(user.id, Duration::from_secs(3600))
            .await
            .unwrap();
        let new_hash = hash_password("new correct horse battery").unwrap();
        assert!(store.change_password(user.id, &new_hash).await.unwrap());
        assert!(
            store
                .authenticate_session(&session.session_token)
                .await
                .unwrap()
                .is_none()
        );
        let credentials = store
            .credentials_by_username("password_user")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.password_hash, new_hash);

        store
            .record_audit(
                Some(user.id),
                "auth.password_changed",
                Some("user"),
                Some(user.id),
                Some("req-test"),
                &serde_json::json!({"source":"test"}),
            )
            .await
            .unwrap();
        let action: String =
            sqlx::query_scalar("SELECT action FROM audit_logs WHERE actor_user_id=?")
                .bind(user.id)
                .fetch_one(store.sqlite_pool())
                .await
                .unwrap();
        assert_eq!(action, "auth.password_changed");
    }

    #[tokio::test]
    async fn persists_and_scopes_run_steps() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let password_hash = hash_password("correct horse battery staple").unwrap();
        let alice = store
            .create_user("steps_alice", &password_hash, "user")
            .await
            .unwrap();
        let bob = store
            .create_user("steps_bob", &password_hash, "user")
            .await
            .unwrap();
        let task = store
            .create_for_owner(alice.id, input("step task"))
            .await
            .unwrap();
        let run = store.start_run(task.id).await.unwrap();
        store
            .record_run_step(&RunStep {
                id: 0,
                run_id: run.id,
                step_index: 0,
                name: "request".into(),
                status: "succeeded".into(),
                http_status: Some(200),
                body_size: 12,
                error: None,
                started_at: 1,
                finished_at: 2,
            })
            .await
            .unwrap();
        assert_eq!(store.list_run_steps(run.id).await.unwrap().len(), 1);
        assert!(
            store
                .list_run_steps_for_owner(run.id, bob.id)
                .await
                .unwrap()
                .is_none()
        );
        let steps = store
            .list_run_steps_for_owner(run.id, alice.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(steps[0].http_status, Some(200));
        assert_eq!(steps[0].body_size, 12);
    }

    #[tokio::test]
    async fn purges_expired_sessions() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let password_hash = hash_password("correct horse battery staple").unwrap();
        let user = store
            .create_user("expiry_user", &password_hash, "user")
            .await
            .unwrap();
        let session = store
            .create_session(user.id, Duration::from_secs(3600))
            .await
            .unwrap();
        sqlx::query("UPDATE sessions SET expires_at=0 WHERE token_hash=?")
            .bind(token_hash(&session.session_token))
            .execute(store.sqlite_pool())
            .await
            .unwrap();

        assert_eq!(store.purge_expired_sessions().await.unwrap(), 1);
        assert!(
            store
                .authenticate_session(&session.session_token)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn records_task_run_history() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let task = store.create(input("run history")).await.unwrap();

        let run = store.start_run(task.id).await.unwrap();
        assert_eq!(run.status, "running");
        store.finish_run(run.id, Some(204), None).await.unwrap();

        let runs = store.list_task_runs(task.id).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "succeeded");
        assert_eq!(runs[0].http_status, Some(204));
        assert!(runs[0].finished_at.is_some());
    }

    #[tokio::test]
    async fn runs_template_crud_and_validates_definitions() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();

        let created = store
            .create_template(CreateTemplate {
                name: "health check".into(),
                description: Some("initial description".into()),
                definition: template_definition("health check"),
                grp: None,
            })
            .await
            .unwrap();
        assert_eq!(created.schema_version, i64::from(TEMPLATE_SCHEMA_VERSION));
        assert_eq!(store.list_templates().await.unwrap().len(), 1);

        let updated = store
            .update_template(
                created.id,
                UpdateTemplate {
                    name: Some("renamed".into()),
                    description: Some("updated description".into()),
                    definition: None,
                    grp: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.description.as_deref(), Some("updated description"));

        let mut invalid = template_definition("invalid");
        invalid.version = TEMPLATE_SCHEMA_VERSION + 1;
        assert!(
            store
                .create_template(CreateTemplate {
                    name: "invalid".into(),
                    description: None,
                    definition: invalid,
                    grp: None,
                })
                .await
                .is_err()
        );

        assert!(store.delete_template(created.id).await.unwrap());
        assert!(store.get_template(created.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn imports_qd_har_without_losing_json() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let har = serde_json::json!({
            "log": {
                "version": "1.2",
                "creator": {"name": "binux", "version": "QD"},
                "custom": [1, 2, 3],
                "entries": [{
                    "checked": true,
                    "request": {"method": "GET", "url": "https://example.invalid", "headers": [], "cookies": []},
                    "success_asserts": [{"re": "200", "from": "status"}],
                    "failed_asserts": [],
                    "extract_variables": []
                }]
            }
        });

        let imported = store
            .import_qd_har(ImportQdHarTemplate {
                name: "legacy QD HAR".into(),
                description: None,
                har: har.clone(),
            })
            .await
            .unwrap();
        assert_eq!(imported.source_format, "qd_har");
        assert_eq!(imported.qd_har, Some(har));
        assert!(imported.definition.is_none());
    }

    #[tokio::test]
    async fn notification_channels_validate_and_scope_owners() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let alice = store
            .create_user("alice-notify", "$argon2id$test", "user")
            .await
            .unwrap();
        let bob = store
            .create_user("bob-notify", "$argon2id$test", "user")
            .await
            .unwrap();
        assert!(
            store
                .create_notification_channel(
                    alice.id,
                    CreateNotificationChannel {
                        name: "unsafe".into(),
                        kind: "webhook".into(),
                        config: serde_json::json!({"url":"http://example.com"}),
                        enabled: true
                    }
                )
                .await
                .is_err()
        );
        let channel = store
            .create_notification_channel(
                alice.id,
                CreateNotificationChannel {
                    name: "deploy".into(),
                    kind: "webhook".into(),
                    config: serde_json::json!({"url":"https://example.com/hook"}),
                    enabled: true,
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .get_notification_channel(channel.id, bob.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .update_notification_channel(
                    channel.id,
                    bob.id,
                    UpdateNotificationChannel {
                        name: Some("stolen".into()),
                        config: None,
                        enabled: None
                    }
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !store
                .delete_notification_channel(channel.id, bob.id)
                .await
                .unwrap()
        );
        let updated = store
            .update_notification_channel(
                channel.id,
                alice.id,
                UpdateNotificationChannel {
                    name: None,
                    config: None,
                    enabled: Some(false),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!updated.enabled);
        assert!(
            store
                .delete_notification_channel(channel.id, alice.id)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn plugins_reject_shell_operators_and_scope_owners() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let alice = store
            .create_user("alice-plugin", "$argon2id$test", "user")
            .await
            .unwrap();
        let bob = store
            .create_user("bob-plugin", "$argon2id$test", "user")
            .await
            .unwrap();
        assert!(
            store
                .create_plugin(
                    alice.id,
                    CreatePluginManifest {
                        name: "bad".into(),
                        command: "echo | sh".into(),
                        config: serde_json::json!({}),
                        enabled: true
                    }
                )
                .await
                .is_err()
        );
        let plugin = store
            .create_plugin(
                alice.id,
                CreatePluginManifest {
                    name: "echo".into(),
                    command: "qdrust-plugin-echo".into(),
                    config: serde_json::json!({}),
                    enabled: true,
                },
            )
            .await
            .unwrap();
        assert!(store.get_plugin(plugin.id, bob.id).await.unwrap().is_none());
        assert!(!store.delete_plugin(plugin.id, bob.id).await.unwrap());
        let updated = store
            .update_plugin(
                plugin.id,
                alice.id,
                UpdatePluginManifest {
                    name: None,
                    command: None,
                    config: None,
                    enabled: Some(false),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!updated.enabled);
        assert!(store.delete_plugin(plugin.id, alice.id).await.unwrap());
    }

    #[tokio::test]
    async fn public_templates_publish_and_copy_as_private_snapshots() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let alice = store
            .create_user("alice-public", "$argon2id$test", "user")
            .await
            .unwrap();
        let bob = store
            .create_user("bob-public", "$argon2id$test", "user")
            .await
            .unwrap();
        let template = store
            .create_template_for_owner(
                alice.id,
                CreateTemplate {
                    name: "shared".into(),
                    description: None,
                    definition: template_definition("shared"),
                    grp: None,
                },
            )
            .await
            .unwrap();
        assert!(store.list_public_templates().await.unwrap().is_empty());
        assert!(
            !store
                .set_template_published(template.id, bob.id, true)
                .await
                .unwrap()
        );
        assert!(
            store
                .set_template_published(template.id, alice.id, true)
                .await
                .unwrap()
        );
        assert_eq!(store.list_public_templates().await.unwrap().len(), 1);
        let copied = store
            .copy_public_template(template.id, bob.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(copied.name, "shared (copy)");
        assert!(
            store
                .get_template_for_owner(copied.id, bob.id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .set_template_published(template.id, alice.id, false)
                .await
                .unwrap()
        );
        assert!(
            store
                .copy_public_template(template.id, bob.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn run_leases_are_competitive_and_recoverable() {
        let store = Store::connect("sqlite::memory:", 1, 4).await.unwrap();
        let task = store
            .create(CreateTask {
                name: "lease".into(),
                cron: "0/5 * * * * * *".into(),
                method: None,
                url: "https://example.invalid".into(),
                headers: Default::default(),
                body: None,
                disabled: false,
                template_id: None,
                grp: None,
                timeout_seconds: None,
                retry_count: None,
                retry_interval_seconds: None,
                priority: None,
                timezone: None,
                random_delay_max_seconds: None,
                variables: None,
            })
            .await
            .unwrap();
        let first = store.enqueue_run(task.id).await.unwrap().unwrap();
        assert!(store.enqueue_run(task.id).await.unwrap().is_none());
        let a = store.claim_run("a", 1).await.unwrap();
        let b = store.claim_run("b", 1).await.unwrap();
        assert_eq!(a.is_some() as u8 + b.is_some() as u8, 1);
        sqlx::query("UPDATE runs SET lease_expires_at=? WHERE id=?")
            .bind(0_i64)
            .bind(first.id)
            .execute(store.sqlite_pool())
            .await
            .unwrap();
        assert_eq!(store.recover_expired_runs().await.unwrap(), 1);
        let recovered = store.get_run(first.id).await.unwrap().unwrap();
        assert_eq!(recovered.status, "pending");
        assert!(store.cancel_run(first.id).await.unwrap());
        assert_eq!(
            store.get_run(first.id).await.unwrap().unwrap().status,
            "cancelled"
        );
    }

    #[tokio::test]
    async fn email_verification_flow_marks_users_verified() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let user = store
            .create_user("verify_me", "$argon2id$test", "user")
            .await
            .unwrap();
        assert!(!user.email_verified);
        assert!(
            store
                .set_user_email(user.id, "A@Example.COM")
                .await
                .unwrap()
        );
        let updated = store.get_user(user.id).await.unwrap().unwrap();
        assert_eq!(updated.email.as_deref(), Some("a@example.com"));
        let (token, _expires) = store
            .create_email_verification_token(user.id, 3600)
            .await
            .unwrap();
        assert!(
            store
                .consume_email_verification_token("bogus")
                .await
                .unwrap()
                .is_none()
        );
        let verified = store
            .consume_email_verification_token(&token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(verified, user.id);
        assert!(
            store
                .get_user(user.id)
                .await
                .unwrap()
                .unwrap()
                .email_verified
        );
    }

    #[tokio::test]
    async fn csrf_rotation_updates_session_hash() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let user = store
            .create_user("csrf_rotate", "$argon2id$test", "user")
            .await
            .unwrap();
        let session = store
            .create_session(user.id, Duration::from_secs(3600))
            .await
            .unwrap();
        let new_hash = "new-hash";
        assert!(
            store
                .rotate_csrf(&session.session_token, new_hash)
                .await
                .unwrap()
        );
        let authenticated = store
            .authenticate_session(&session.session_token)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(authenticated.csrf_token_hash, new_hash);
    }

    #[tokio::test]
    async fn subscriptions_crud_and_sync_records() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let user = store
            .create_user("sub_owner", "$argon2id$test", "user")
            .await
            .unwrap();
        let created = store
            .create_subscription(
                user.id,
                CreateTemplateSubscription {
                    name: "qd-templates".into(),
                    url: "https://github.com/example/qd-templates".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(created.name, "qd-templates");
        assert!(created.enabled);
        assert_eq!(store.list_subscriptions(user.id).await.unwrap().len(), 1);
        assert!(
            store
                .get_subscription(created.id, user.id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_subscription(created.id, user.id + 999)
                .await
                .unwrap()
                .is_none()
        );
        let sync = store.create_subscription_sync(created.id).await.unwrap();
        assert_eq!(sync.status, "pending");
        store
            .finish_subscription_sync(sync.id, "succeeded", Some("imported 3"))
            .await
            .unwrap();
        let synced = store.get_subscription_sync(sync.id).await.unwrap().unwrap();
        assert_eq!(synced.status, "succeeded");
        assert_eq!(synced.message.as_deref(), Some("imported 3"));
        assert!(
            store
                .delete_subscription(created.id, user.id)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn push_requests_require_approval_to_publish() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let alice = store
            .create_user("push_alice", "$argon2id$test", "user")
            .await
            .unwrap();
        let admin = store
            .create_user("push_admin", "$argon2id$test", "admin")
            .await
            .unwrap();
        let template = store
            .create_template_for_owner(
                alice.id,
                CreateTemplate {
                    name: "to-publish".into(),
                    description: None,
                    definition: template_definition("to-publish"),
                    grp: None,
                },
            )
            .await
            .unwrap();
        assert!(store.list_public_templates().await.unwrap().is_empty());
        let request = store
            .create_push_request(
                alice.id,
                CreatePushRequest {
                    template_id: template.id,
                    note: Some("please review".into()),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.status, "pending");
        assert_eq!(
            store
                .list_push_requests_for_owner(alice.id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list_push_requests(Some("pending"))
                .await
                .unwrap()
                .len(),
            1
        );
        let decided = store
            .decide_push_request(
                request.id,
                admin.id,
                &DecidePushRequest {
                    approve: true,
                    note: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(decided.status, "approved");
        assert!(store.list_public_templates().await.unwrap().len() == 1);
    }

    #[tokio::test]
    async fn delayed_enqueue_becomes_claimable_only_after_run_after() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let task = store.create(input("delayed")).await.unwrap();
        assert_eq!(
            store
                .get(task.id)
                .await
                .unwrap()
                .unwrap()
                .random_delay_max_seconds,
            Some(0)
        );

        let run = store
            .enqueue_delayed_run(task.id, 60)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.status, "pending");
        assert!(run.run_after.is_some());
        // The jitter has not elapsed yet: the worker must not claim the run.
        assert!(store.claim_run("worker", 300).await.unwrap().is_none());
        // Once run_after passes (simulated), the pending run is claimable.
        sqlx::query("UPDATE runs SET run_after=0 WHERE id=?")
            .bind(run.id)
            .execute(store.sqlite_pool())
            .await
            .unwrap();
        assert!(store.claim_run("worker", 300).await.unwrap().is_some());
        // The one-active-run guard still applies for delayed enqueues.
        assert!(
            store
                .enqueue_delayed_run(task.id, 60)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn backup_and_restore_round_trips_data() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let user = store
            .create_user("backup_user", "$argon2id$test", "user")
            .await
            .unwrap();
        let task = store
            .create_for_owner(
                user.id,
                CreateTask {
                    name: "backup task".into(),
                    cron: "0 * * * * * *".into(),
                    method: Some("GET".into()),
                    url: "https://example.com".into(),
                    headers: Default::default(),
                    body: None,
                    disabled: false,
                    template_id: None,
                    grp: Some("prod".into()),
                    timeout_seconds: None,
                    retry_count: None,
                    retry_interval_seconds: None,
                    priority: None,
                    timezone: None,
                    random_delay_max_seconds: Some(30),
                    variables: None,
                },
            )
            .await
            .unwrap();
        let backup = store.export_data().await.unwrap();
        assert!(backup.get("users").is_some());
        assert!(backup.get("tasks").is_some());

        let target = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        target.import_data(&backup).await.unwrap();
        let restored = target
            .get_for_owner(task.id, user.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.name, "backup task");
        assert_eq!(restored.grp.as_deref(), Some("prod"));
        assert_eq!(target.list().await.unwrap().len(), 1);
        assert_eq!(target.list_users().await.unwrap().len(), 1);
    }
}
