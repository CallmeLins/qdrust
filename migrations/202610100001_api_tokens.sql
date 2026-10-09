-- Personal access tokens: the programmatic credential MCP clients and scripts
-- use, so a machine can call the API without a browser session and its CSRF
-- cookie. See docs/authentication.md.
--
-- Only the SHA-256 hash is stored; the plaintext is returned once at creation
-- and never again. `expires_at` is nullable (no expiry), and `last_used_at` is
-- touched best-effort so an operator can spot a dead token. Revoking is a
-- delete: there is nothing else to keep.
--
-- No scopes yet: a token carries its owner's full API access, so it is a
-- password-grade secret. The WebUI says so, and creating or revoking one writes
-- an `api_token.created` / `api_token.revoked` audit record.
CREATE TABLE api_tokens (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    expires_at INTEGER
);

CREATE INDEX idx_api_tokens_user ON api_tokens(user_id);
