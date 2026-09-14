-- Subscription import mode. `all` keeps the original behaviour of pulling in
-- every template a source offers; `select` treats the source as a browsable
-- library and imports only the entries the user picks. The official library
-- (https://github.com/qd-today/templates) ships hundreds of entries, so
-- auto-importing all of them would bury a user's own templates.
ALTER TABLE template_subscriptions ADD COLUMN mode VARCHAR(16) NOT NULL DEFAULT 'all';

-- Which subscription entry a local template was imported from, and at which
-- upstream version. The library browser joins on this to mark an entry as
-- already imported and to offer an update once the upstream version advances.
-- Deleting a subscription drops the provenance but keeps the imported
-- templates: an import is a copy into the user's library, not a link.
CREATE TABLE template_imports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription_id INTEGER NOT NULL REFERENCES template_subscriptions(id) ON DELETE CASCADE,
    template_id INTEGER NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
    entry_name TEXT NOT NULL,
    entry_version VARCHAR(32),
    source_url TEXT,
    imported_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (subscription_id, entry_name)
);
CREATE INDEX idx_template_imports_subscription ON template_imports(subscription_id, entry_name);
CREATE INDEX idx_template_imports_template ON template_imports(template_id);
