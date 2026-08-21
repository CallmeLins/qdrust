-- Task/template grouping (QD _groups equivalent)
ALTER TABLE tasks ADD COLUMN grp TEXT;
ALTER TABLE templates ADD COLUMN grp TEXT;
CREATE INDEX idx_tasks_owner_grp ON tasks(owner_id, grp);
CREATE INDEX idx_templates_owner_grp ON templates(owner_id, grp);

-- Site settings (admin configurable key/value store)
CREATE TABLE site_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Password reset tokens (email-based reset)
CREATE TABLE password_reset_tokens (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX idx_password_reset_user ON password_reset_tokens(user_id, created_at);
