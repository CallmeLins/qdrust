-- External identity login (OIDC / Header Auth) for Issue #3.
-- See docs/EXTERNAL_IDP_PLAN.md §3.

-- Maps an external identity (provider + issuer + subject) to a local user row.
-- Mapping key is provider+issuer+subject, NOT email (email may change, may be
-- shared, and is not unique).  An external user is provisioned into `users`
-- with an un-loggable sentinel argon2 hash so the local credential code path
-- (which requires a NON-NULL password_hash) stays untouched.
CREATE TABLE external_identities (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider      TEXT NOT NULL,             -- 'oidc' | 'header'
    issuer        TEXT NOT NULL DEFAULT '',  -- OIDC issuer; '' for header mode
    subject       TEXT NOT NULL,             -- OIDC sub / normalized Remote-User
    email         TEXT,
    created_at    INTEGER NOT NULL,
    last_login_at INTEGER NOT NULL,
    UNIQUE (provider, issuer, subject)
);
CREATE INDEX idx_external_identities_user ON external_identities(user_id);

-- OIDC authorization state must survive restarts and multi-instance deploys,
-- so the browser callback can land on any replica.  Redis (when configured)
-- is preferred; this table is the DB fallback and the single-instance store.
-- pkce_verifier is sensitive (redeemable for tokens) so it is stored encrypted.
CREATE TABLE oidc_login_states (
    state_hash              TEXT PRIMARY KEY,
    nonce_hash              TEXT NOT NULL,
    pkce_verifier_encrypted TEXT NOT NULL,
    redirect_uri            TEXT NOT NULL,
    created_at              INTEGER NOT NULL,
    expires_at              INTEGER NOT NULL
);
CREATE INDEX idx_oidc_login_states_expiry ON oidc_login_states(expires_at);
