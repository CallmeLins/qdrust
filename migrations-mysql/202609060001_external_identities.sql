-- Parity with the SQLite 202609060001_external_identities migration
-- (Issue #3, see docs/EXTERNAL_IDP_PLAN.md §3).
-- SQLite uses dynamic-typed columns; here we use explicit MySQL types:
--   unix-epoch timestamps -> BIGINT, strings -> VARCHAR/TEXT, booleans -> TINYINT(1).

CREATE TABLE external_identities (
    id            BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    user_id       BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider      VARCHAR(32) NOT NULL,
    issuer        VARCHAR(255) NOT NULL DEFAULT '',
    subject       VARCHAR(255) NOT NULL,
    email         VARCHAR(255),
    created_at    BIGINT NOT NULL,
    last_login_at BIGINT NOT NULL,
    UNIQUE KEY uq_external_identity (provider, issuer, subject),
    KEY idx_external_identities_user (user_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE oidc_login_states (
    state_hash              VARCHAR(128) PRIMARY KEY,
    nonce_hash              VARCHAR(128) NOT NULL,
    pkce_verifier_encrypted TEXT NOT NULL,
    redirect_uri            TEXT NOT NULL,
    created_at              BIGINT NOT NULL,
    expires_at              BIGINT NOT NULL,
    KEY idx_oidc_login_states_expiry (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
