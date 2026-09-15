-- Subscriptions are sources the user browses and imports from on demand, and
-- that is now the only way they work. The import mode (`select` / `all`) is
-- gone: nothing imports a whole source behind the user's back any more, so the
-- picker that chose between the two has nothing to choose.
--
-- The pieces that only existed to serve `all` go with it: the periodic sync
-- pass, the manual sync endpoint, and the sync history. `last_synced_at` and
-- `last_error` were written by nothing else, and a source that cannot be read
-- reports that on the request that tried, not in a column.
ALTER TABLE template_subscriptions DROP COLUMN mode;
ALTER TABLE template_subscriptions DROP COLUMN last_synced_at;
ALTER TABLE template_subscriptions DROP COLUMN last_error;

DROP TABLE subscription_syncs;
