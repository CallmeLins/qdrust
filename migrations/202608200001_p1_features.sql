-- Email verification (MustVerifyEmail)
ALTER TABLE users ADD COLUMN email TEXT;
ALTER TABLE users ADD COLUMN email_verified INTEGER NOT NULL DEFAULT 0;

CREATE TABLE email_verification_tokens (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX idx_email_verification_user ON email_verification_tokens(user_id, created_at);

-- Public template subscriptions (subscribe repositories / template sources)
CREATE TABLE template_subscriptions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    last_synced_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_subscriptions_owner ON template_subscriptions(owner_id, id);

-- Subscription sync runs (progress reporting to WebSocket clients)
CREATE TABLE subscription_syncs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription_id INTEGER NOT NULL REFERENCES template_subscriptions(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'succeeded', 'failed')),
    message TEXT,
    created_at INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX idx_subscription_syncs ON subscription_syncs(subscription_id, id);

-- Push requests (template publication approval workflow)
CREATE TABLE push_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    template_id INTEGER NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected')),
    note TEXT,
    reviewed_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    reviewed_at INTEGER,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_push_requests_owner ON push_requests(owner_id, created_at DESC, id DESC);
CREATE INDEX idx_push_requests_status ON push_requests(status, created_at DESC, id DESC);
