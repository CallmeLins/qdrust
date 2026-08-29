-- Widen notification channel kinds: add the 8 QD-style push channels
-- (bark, serverchan, telegram, dingtalk, wxpusher, wxpusher_spt,
-- wecom_app, wecom_webhook) alongside the existing webhook and email.
-- SQLite cannot alter a CHECK constraint, so rebuild the table and copy
-- existing rows over.
CREATE TABLE notification_channels_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('webhook', 'email', 'bark', 'serverchan', 'telegram', 'dingtalk', 'wxpusher', 'wxpusher_spt', 'wecom_app', 'wecom_webhook')),
    config TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
INSERT INTO notification_channels_new (id, owner_id, name, kind, config, enabled, created_at, updated_at)
    SELECT id, owner_id, name, kind, config, enabled, created_at, updated_at FROM notification_channels;
DROP TABLE notification_channels;
ALTER TABLE notification_channels_new RENAME TO notification_channels;
CREATE INDEX idx_notification_channels_owner ON notification_channels(owner_id, id);
