-- Parity with the SQLite 202610100001_api_tokens migration: personal access
-- tokens for programmatic API access. See that file for the design and the
-- "no scopes yet" note.
--
-- No foreign key, matching every other table in this directory.
CREATE TABLE api_tokens (
    id BIGINT NOT NULL AUTO_INCREMENT,
    user_id BIGINT NOT NULL,
    name VARCHAR(128) NOT NULL,
    token_hash VARCHAR(128) NOT NULL,
    created_at BIGINT NOT NULL,
    last_used_at BIGINT NULL,
    expires_at BIGINT NULL,
    PRIMARY KEY (id),
    UNIQUE KEY uk_api_tokens_hash (token_hash),
    KEY idx_api_tokens_user (user_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
