-- Optional second factor for local username/password logins.
--
-- The secret is stored as the base32 string the authenticator app was given, not
-- hashed: TOTP verification recomputes the code from it on every attempt, so a
-- one-way hash would make it useless. That makes the column as sensitive as a
-- password and it never leaves the server -- `GET /auth/session` and the admin
-- user list do not select it, and the only reads are the verification path.
--
-- `totp_enabled` is separate from a non-null secret so enrolment can be
-- abandoned: setup writes the secret, and only a code the user proved from their
-- own app flips the flag. Until then login behaves exactly as before.
ALTER TABLE users ADD COLUMN totp_secret TEXT;
ALTER TABLE users ADD COLUMN totp_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN totp_confirmed_at INTEGER;

-- A password that checks out but has not yet been through the second factor
-- leaves its credentials here instead of a session: a row that expires in
-- minutes, can be retried only a few times, and is worthless without the code.
-- Postgres-shaped short-lived state would be a cookie; a table was chosen
-- because it also lets an administrator see and revoke a stuck attempt.
CREATE TABLE totp_login_challenges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX idx_totp_login_challenges_user ON totp_login_challenges(user_id);
