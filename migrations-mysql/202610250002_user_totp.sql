-- Optional second factor for local logins; see the SQLite sibling for why the
-- secret is not hashed and why the flag is separate from the secret.
--
-- MySQL carries no foreign keys in this project (the inline REFERENCES are
-- parsed and ignored), so `delete_user` removes these rows by hand -- and so
-- does the expired-challenge sweep.
ALTER TABLE users ADD COLUMN totp_secret TEXT;
ALTER TABLE users ADD COLUMN totp_enabled TINYINT NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN totp_confirmed_at BIGINT NULL;

CREATE TABLE totp_login_challenges (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    user_id BIGINT NOT NULL,
    token_hash VARCHAR(128) NOT NULL,
    attempts BIGINT NOT NULL DEFAULT 0,
    created_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    UNIQUE KEY idx_totp_login_challenges_token (token_hash),
    INDEX idx_totp_login_challenges_user (user_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
