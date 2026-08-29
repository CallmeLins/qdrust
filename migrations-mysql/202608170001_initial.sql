-- qdrust consolidated schema for MySQL (fresh databases only).
-- Mirrors the SQLite migration chain; all timestamps are unix epoch BIGINTs,
-- boolean columns are TINYINT(1) 0/1, JSON payloads are LONGTEXT.

CREATE TABLE users (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    username VARCHAR(64) NOT NULL UNIQUE,
    password_hash VARCHAR(255) NOT NULL,
    role VARCHAR(16) NOT NULL DEFAULT 'user',
    disabled TINYINT(1) NOT NULL DEFAULT 0,
    session_version BIGINT NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE sessions (
    token_hash VARCHAR(128) PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_token_hash VARCHAR(128) NOT NULL,
    session_version BIGINT NOT NULL,
    created_at BIGINT NOT NULL,
    last_seen_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    INDEX idx_sessions_user (user_id),
    INDEX idx_sessions_expiry (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE audit_logs (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    actor_user_id BIGINT REFERENCES users(id) ON DELETE SET NULL,
    action VARCHAR(128) NOT NULL,
    resource_type VARCHAR(64),
    resource_id BIGINT,
    request_id VARCHAR(64),
    details LONGTEXT NOT NULL,
    created_at BIGINT NOT NULL,
    INDEX idx_audit_logs_actor_created (actor_user_id, created_at DESC, id DESC),
    INDEX idx_audit_logs_resource (resource_type, resource_id, created_at DESC)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE templates (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    schema_version BIGINT NOT NULL DEFAULT 1,
    definition LONGTEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    source_format VARCHAR(32) NOT NULL DEFAULT 'native_v1',
    source TEXT,
    owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
    published TINYINT(1) NOT NULL DEFAULT 0,
    grp VARCHAR(128),
    INDEX idx_templates_owner_updated (owner_id, updated_at DESC, id DESC),
    INDEX idx_templates_public (published, updated_at DESC)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE tasks (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    template_id BIGINT REFERENCES templates(id) ON DELETE RESTRICT,
    name VARCHAR(255) NOT NULL,
    cron VARCHAR(255) NOT NULL,
    method VARCHAR(16) NOT NULL,
    url TEXT NOT NULL,
    headers LONGTEXT NOT NULL,
    body LONGTEXT,
    disabled TINYINT(1) NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    last_run_at BIGINT,
    last_status BIGINT,
    last_error TEXT,
    owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
    grp VARCHAR(128),
    INDEX idx_tasks_due (disabled, last_run_at),
    INDEX idx_tasks_owner_grp (owner_id, grp),
    INDEX idx_tasks_owner_due (owner_id, disabled, last_run_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE runs (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    task_id BIGINT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    status VARCHAR(16) NOT NULL,
    http_status BIGINT,
    error TEXT,
    started_at BIGINT,
    finished_at BIGINT,
    created_at BIGINT NOT NULL,
    lease_owner VARCHAR(64),
    lease_expires_at BIGINT,
    attempt BIGINT NOT NULL DEFAULT 0,
    cancel_requested TINYINT(1) NOT NULL DEFAULT 0,
    INDEX idx_runs_task_created (task_id, created_at DESC, id DESC),
    INDEX idx_runs_claim (status, lease_expires_at, created_at, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE run_steps (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    run_id BIGINT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    step_index BIGINT NOT NULL,
    name VARCHAR(255) NOT NULL,
    status VARCHAR(16) NOT NULL,
    http_status BIGINT,
    body_size BIGINT NOT NULL DEFAULT 0,
    error TEXT,
    started_at BIGINT NOT NULL,
    finished_at BIGINT NOT NULL,
    UNIQUE INDEX idx_run_steps_order (run_id, step_index)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE notes (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title VARCHAR(255) NOT NULL,
    content LONGTEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    INDEX idx_notes_owner_updated (owner_id, updated_at DESC, id DESC)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE notification_channels (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    kind VARCHAR(16) NOT NULL,
    config LONGTEXT NOT NULL,
    enabled TINYINT(1) NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    INDEX idx_notification_channels_owner (owner_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE notification_actions (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    task_id BIGINT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    channel_id BIGINT NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    event VARCHAR(16) NOT NULL,
    created_at BIGINT NOT NULL,
    UNIQUE INDEX idx_notification_actions_unique (task_id, channel_id, event),
    INDEX idx_notification_actions_task (task_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE plugins (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    command TEXT NOT NULL,
    config LONGTEXT NOT NULL,
    enabled TINYINT(1) NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE INDEX idx_plugins_owner_name (owner_id, name),
    INDEX idx_plugins_owner (owner_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE site_settings (
    `key` VARCHAR(128) PRIMARY KEY,
    value LONGTEXT NOT NULL,
    updated_at BIGINT NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE password_reset_tokens (
    token_hash VARCHAR(128) PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    INDEX idx_password_reset_user (user_id, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- P1 features: email verification, subscriptions, sync runs, push requests
ALTER TABLE users ADD COLUMN email VARCHAR(255);
ALTER TABLE users ADD COLUMN email_verified TINYINT(1) NOT NULL DEFAULT 0;

CREATE TABLE email_verification_tokens (
    token_hash VARCHAR(128) PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    INDEX idx_email_verification_user (user_id, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE template_subscriptions (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    url VARCHAR(2048) NOT NULL,
    enabled TINYINT(1) NOT NULL DEFAULT 1,
    last_synced_at BIGINT,
    last_error TEXT,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    INDEX idx_subscriptions_owner (owner_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE subscription_syncs (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    subscription_id BIGINT NOT NULL REFERENCES template_subscriptions(id) ON DELETE CASCADE,
    status VARCHAR(16) NOT NULL,
    message TEXT,
    created_at BIGINT NOT NULL,
    finished_at BIGINT,
    INDEX idx_subscription_syncs (subscription_id, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE push_requests (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    template_id BIGINT NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    note TEXT,
    reviewed_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    reviewed_at BIGINT,
    created_at BIGINT NOT NULL,
    INDEX idx_push_requests_owner (owner_id, created_at DESC, id DESC),
    INDEX idx_push_requests_status (status, created_at DESC, id DESC)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
