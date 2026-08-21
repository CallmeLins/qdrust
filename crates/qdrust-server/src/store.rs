use std::{path::Path, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow, ensure};
use chrono::Utc;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::auth::{new_token, token_hash};
use crate::model::{
    AdminUserUpdate, AuthenticatedSession, BatchTaskOperation, CreateNote,
    CreateNotificationAction, CreateNotificationChannel, CreatePluginManifest, CreateTask,
    CreateTemplate, ImportQdHarTemplate, IssuedSession, Note, NotificationAction,
    NotificationChannel, PluginManifest, Run, RunStep, SetSiteSetting, SiteSetting, Task, Template,
    UpdateNote, UpdateNotificationChannel, UpdatePluginManifest, UpdateQdHarTemplate, UpdateTask,
    UpdateTemplate, User, UserCredentials,
};
use qdrust_core::{qd_har::QdHar, template::TEMPLATE_SCHEMA_VERSION};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
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
        ensure_sqlite_parent(url)?;
        let options = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .min_connections(min_connections)
            .max_connections(max_connections)
            .acquire_timeout(acquire_timeout)
            .idle_timeout(idle_timeout)
            .connect_with(options)
            .await?;
        sqlx::migrate!("../../migrations").run(&pool).await?;
        Ok(Self { pool })
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
        let result = sqlx::query(
            "INSERT INTO users(username, password_hash, role, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(username.trim())
        .bind(password_hash)
        .bind(role)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.get_user(result.last_insert_rowid())
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
        let result = sqlx::query(
            "INSERT INTO users(username,password_hash,role,created_at,updated_at)
             VALUES (?,?,'admin',?,?)",
        )
        .bind(username.trim())
        .bind(password_hash)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        let id = result.last_insert_rowid();
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
        let row = sqlx::query(
            "SELECT id,username,password_hash,role,disabled,session_version,created_at,updated_at
             FROM users WHERE username = ? COLLATE NOCASE",
        )
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
            "SELECT u.id,u.username,u.role,u.disabled,u.created_at,u.updated_at,
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
            "SELECT id,username,password_hash,role,disabled,session_version,created_at,updated_at
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
        validate(&input.name, &input.cron, &input.url)?;
        let now = Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT INTO tasks(name, cron, method, url, headers, body, disabled, created_at, updated_at, owner_id, template_id, grp)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .execute(&self.pool)
        .await?;
        self.get(result.last_insert_rowid())
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
        validate(&name, &cron, &url)?;
        let headers = input
            .headers
            .map(serde_json::Value::Object)
            .unwrap_or(current.headers);
        let grp = match input.grp {
            Some(grp) => grp,
            None => current.grp,
        };
        sqlx::query(
            "UPDATE tasks SET name=?, cron=?, method=?, url=?, headers=?, body=?, disabled=?, template_id=?, grp=?, updated_at=? WHERE id=?",
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
        let result = sqlx::query(
            "INSERT INTO runs(task_id,status,started_at,created_at,attempt)
             VALUES (?,'running',?,?,1)",
        )
        .bind(task_id)
        .bind(started_at)
        .bind(started_at)
        .execute(&self.pool)
        .await?;
        self.get_run(result.last_insert_rowid())
            .await?
            .context("created run disappeared")
    }

    pub async fn enqueue_run(&self, task_id: i64) -> Result<Option<Run>> {
        let now = Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT INTO runs(task_id,status,created_at,attempt) VALUES (?,'pending',?,0)",
        )
        .bind(task_id)
        .bind(now)
        .execute(&self.pool)
        .await;
        match result {
            Ok(result) => self.get_run(result.last_insert_rowid()).await,
            Err(error) if error.to_string().contains("UNIQUE") => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn claim_run(&self, worker: &str, lease_seconds: i64) -> Result<Option<Run>> {
        let now = Utc::now().timestamp();
        let expires = now + lease_seconds.max(1);
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT id FROM runs WHERE (status='pending' OR (status IN ('leased','running') AND lease_expires_at<=?)) AND cancel_requested=0 ORDER BY created_at,id LIMIT 1")
            .bind(now).fetch_optional(&mut *tx).await?;
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

    pub async fn create_note(&self, owner_id: i64, input: CreateNote) -> Result<Note> {
        ensure!(!input.title.trim().is_empty(), "note title cannot be empty");
        let now = Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT INTO notes(owner_id,title,content,created_at,updated_at) VALUES (?,?,?,?,?)",
        )
        .bind(owner_id)
        .bind(input.title.trim())
        .bind(input.content)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.get_note(result.last_insert_rowid(), owner_id)
            .await?
            .context("created note disappeared")
    }

    pub async fn list_notes(&self, owner_id: i64) -> Result<Vec<Note>> {
        let rows = sqlx::query("SELECT id,title,content,created_at,updated_at FROM notes WHERE owner_id=? ORDER BY updated_at DESC,id DESC")
            .bind(owner_id).fetch_all(&self.pool).await?;
        rows.into_iter().map(note_from_row).collect()
    }

    pub async fn get_note(&self, id: i64, owner_id: i64) -> Result<Option<Note>> {
        let row = sqlx::query(
            "SELECT id,title,content,created_at,updated_at FROM notes WHERE id=? AND owner_id=?",
        )
        .bind(id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(note_from_row).transpose()
    }

    pub async fn update_note(
        &self,
        id: i64,
        owner_id: i64,
        input: UpdateNote,
    ) -> Result<Option<Note>> {
        let Some(current) = self.get_note(id, owner_id).await? else {
            return Ok(None);
        };
        let title = input.title.unwrap_or(current.title);
        ensure!(!title.trim().is_empty(), "note title cannot be empty");
        sqlx::query("UPDATE notes SET title=?,content=?,updated_at=? WHERE id=? AND owner_id=?")
            .bind(title.trim())
            .bind(input.content.unwrap_or(current.content))
            .bind(Utc::now().timestamp())
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await?;
        self.get_note(id, owner_id).await
    }

    pub async fn delete_note(&self, id: i64, owner_id: i64) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM notes WHERE id=? AND owner_id=?")
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            > 0)
    }

    pub async fn create_notification_channel(
        &self,
        owner_id: i64,
        input: CreateNotificationChannel,
    ) -> Result<NotificationChannel> {
        validate_notification(&input.name, &input.kind, &input.config)?;
        let now = Utc::now().timestamp();
        let result = sqlx::query("INSERT INTO notification_channels(owner_id,name,kind,config,enabled,created_at,updated_at) VALUES (?,?,?,?,?,?,?)")
            .bind(owner_id).bind(input.name.trim()).bind(input.kind).bind(serde_json::to_string(&input.config)?).bind(input.enabled).bind(now).bind(now).execute(&self.pool).await?;
        self.get_notification_channel(result.last_insert_rowid(), owner_id)
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
        let result = sqlx::query("INSERT INTO notification_actions(task_id,channel_id,event,created_at) VALUES (?,?,?,?)")
            .bind(task_id).bind(input.channel_id).bind(input.event).bind(Utc::now().timestamp()).execute(&self.pool).await?;
        self.get_notification_action(result.last_insert_rowid(), owner_id)
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
        let result=sqlx::query("INSERT INTO plugins(owner_id,name,command,config,enabled,created_at,updated_at) VALUES(?,?,?,?,?,?,?)").bind(owner_id).bind(input.name.trim()).bind(input.command).bind(serde_json::to_string(&input.config)?).bind(input.enabled).bind(now).bind(now).execute(&self.pool).await?;
        self.get_plugin(result.last_insert_rowid(), owner_id)
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
        let result = sqlx::query(
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
        .execute(&self.pool)
        .await?;
        self.get_template(result.last_insert_rowid())
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
        let result = sqlx::query(
            "INSERT INTO templates(name, description, schema_version, definition, source_format, source, created_at, updated_at, owner_id)
             VALUES (?, ?, 1, '{}', 'qd_har', ?, ?, ?, ?)",
        )
        .bind(input.name)
        .bind(input.description)
        .bind(serde_json::to_string(&input.har)?)
        .bind(now)
        .bind(now)
        .bind(owner_id)
        .execute(&self.pool)
        .await?;
        self.get_template(result.last_insert_rowid())
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
        let result=sqlx::query("INSERT INTO templates(name,description,schema_version,source_format,definition,source,created_at,updated_at,owner_id,published) VALUES(?,?,?,?,?,?,?,?,?,0)").bind(format!("{} (copy)",row.try_get::<String,_>("name")?)).bind(row.try_get::<Option<String>,_>("description")?).bind(row.try_get::<i64,_>("schema_version")?).bind(row.try_get::<String,_>("source_format")?).bind(row.try_get::<String,_>("definition")?).bind(row.try_get::<Option<String>,_>("source")?).bind(now).bind(now).bind(owner_id).execute(&self.pool).await?;
        self.get_template_for_owner(result.last_insert_rowid(), owner_id)
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

    // ---- Site settings ----

    pub async fn get_setting(&self, key: &str) -> Result<Option<SiteSetting>> {
        let row = sqlx::query("SELECT key,value,updated_at FROM site_settings WHERE key=?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        row.map(setting_from_row).transpose()
    }

    pub async fn set_setting(&self, key: &str, input: &SetSiteSetting) -> Result<SiteSetting> {
        sqlx::query(
            "INSERT INTO site_settings(key,value,updated_at) VALUES(?,?,?) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        )
        .bind(key)
        .bind(serde_json::to_string(&input.value)?)
        .bind(Utc::now().timestamp())
        .execute(&self.pool)
        .await?;
        self.get_setting(key).await?.context("setting disappeared")
    }

    pub async fn list_settings(&self) -> Result<Vec<SiteSetting>> {
        let rows = sqlx::query("SELECT key,value,updated_at FROM site_settings ORDER BY key")
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
}

fn setting_from_row(row: sqlx::sqlite::SqliteRow) -> Result<SiteSetting> {
    Ok(SiteSetting {
        key: row.try_get("key")?,
        value: serde_json::from_str(&row.try_get::<String, _>("value")?)?,
        updated_at: row.try_get("updated_at")?,
    })
}

const TASK_FIELDS: &str = "SELECT id,name,cron,method,url,headers,body,disabled,created_at,updated_at,last_run_at,last_status,last_error,template_id,grp FROM tasks";
const TEMPLATE_FIELDS: &str = "SELECT id,name,description,schema_version,definition,source_format,source,created_at,updated_at,grp FROM templates";
const RUN_FIELDS: &str = "SELECT id,task_id,status,http_status,error,started_at,finished_at,created_at,lease_owner,lease_expires_at,attempt,cancel_requested FROM runs";
const USER_FIELDS: &str = "SELECT id,username,role,disabled,created_at,updated_at FROM users";

fn user_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<User> {
    Ok(User {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        role: row.try_get("role")?,
        disabled: row.try_get("disabled")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn credentials_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<UserCredentials> {
    Ok(UserCredentials {
        user: user_from_row(row)?,
        password_hash: row.try_get("password_hash")?,
        session_version: row.try_get("session_version")?,
    })
}

fn authenticated_session_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<AuthenticatedSession> {
    Ok(AuthenticatedSession {
        user: user_from_row(row)?,
        csrf_token_hash: row.try_get("csrf_token_hash")?,
        expires_at: row.try_get("expires_at")?,
    })
}

fn task_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Task> {
    let headers: String = row.try_get("headers")?;
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
    })
}

fn run_step_from_row(row: sqlx::sqlite::SqliteRow) -> Result<RunStep> {
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

fn template_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Template> {
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

fn run_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Run> {
    Ok(Run {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        status: row.try_get("status")?,
        http_status: row.try_get("http_status")?,
        error: row.try_get("error")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
        created_at: row.try_get("created_at")?,
        lease_owner: row.try_get("lease_owner")?,
        lease_expires_at: row.try_get("lease_expires_at")?,
        attempt: row.try_get("attempt")?,
        cancel_requested: row.try_get("cancel_requested")?,
    })
}

fn note_from_row(row: sqlx::sqlite::SqliteRow) -> Result<Note> {
    Ok(Note {
        id: row.try_get("id")?,
        title: row.try_get("title")?,
        content: row.try_get("content")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn notification_from_row(row: sqlx::sqlite::SqliteRow) -> Result<NotificationChannel> {
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

fn notification_action_from_row(row: sqlx::sqlite::SqliteRow) -> Result<NotificationAction> {
    Ok(NotificationAction {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        channel_id: row.try_get("channel_id")?,
        event: row.try_get("event")?,
        created_at: row.try_get("created_at")?,
    })
}

fn plugin_from_row(row: sqlx::sqlite::SqliteRow) -> Result<PluginManifest> {
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

fn validate(name: &str, schedule: &str, url: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(anyhow!("name cannot be empty"));
    }
    schedule
        .parse::<cron::Schedule>()
        .context("invalid cron expression")?;
    reqwest::Url::parse(url).context("invalid task URL")?;
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

fn ensure_sqlite_parent(url: &str) -> Result<()> {
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
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "renamed");
        assert!(updated.disabled);
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
                .fetch_one(&store.pool)
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
                .fetch_one(&store.pool)
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
            .execute(&store.pool)
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
    async fn notes_are_owner_scoped_across_crud() {
        let store = Store::connect("sqlite::memory:", 1, 1).await.unwrap();
        let alice = store
            .create_user("alice-notes", "$argon2id$test", "user")
            .await
            .unwrap();
        let bob = store
            .create_user("bob-notes", "$argon2id$test", "user")
            .await
            .unwrap();
        let note = store
            .create_note(
                alice.id,
                CreateNote {
                    title: "secret".into(),
                    content: "alpha".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(store.list_notes(alice.id).await.unwrap().len(), 1);
        assert!(store.get_note(note.id, bob.id).await.unwrap().is_none());
        assert!(
            store
                .update_note(
                    note.id,
                    bob.id,
                    UpdateNote {
                        title: Some("stolen".into()),
                        content: None
                    }
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(!store.delete_note(note.id, bob.id).await.unwrap());
        let updated = store
            .update_note(
                note.id,
                alice.id,
                UpdateNote {
                    title: None,
                    content: Some("beta".into()),
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.content, "beta");
        assert!(store.delete_note(note.id, alice.id).await.unwrap());
        assert!(store.get_note(note.id, alice.id).await.unwrap().is_none());
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
            .execute(&store.pool)
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
}
