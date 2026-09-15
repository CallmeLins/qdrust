-- See the SQLite migration of the same name: subscriptions are browsed and
-- imported on demand, so the import mode and everything that only existed to
-- auto-import a whole source are gone.
ALTER TABLE template_subscriptions DROP COLUMN mode;
ALTER TABLE template_subscriptions DROP COLUMN last_synced_at;
ALTER TABLE template_subscriptions DROP COLUMN last_error;

DROP TABLE IF EXISTS subscription_syncs;
