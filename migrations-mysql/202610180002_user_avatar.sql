-- Parity with the SQLite 202610180002_user_avatar migration: a whitelisted
-- built-in avatar id only. Uploaded images and arbitrary URLs are unsupported.
ALTER TABLE users
    ADD COLUMN avatar_id VARCHAR(32) NOT NULL DEFAULT 'avatar-01';
