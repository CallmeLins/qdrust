-- One row per (run, action, channel, attempt): the delivery ledger.
--
-- Before this table the only trace of a notification was a tracing line, so
-- "the sign-in silent-failed" had no answer beyond reading container logs that
-- a restart had already rotated away. The row is written after every attempt,
-- successful or not, and the channel kind is copied in so the history stays
-- readable after the channel itself is deleted.
--
-- `action_id` / `channel_id` are deliberately *not* foreign keys: deleting a
-- channel must not erase the record that it once delivered (or failed to
-- deliver) a notification. Rows are removed with their run, which is what the
-- delete paths already do for `run_steps`.
CREATE TABLE notification_deliveries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    task_id INTEGER NOT NULL,
    action_id INTEGER,
    channel_id INTEGER,
    channel_kind TEXT NOT NULL,
    event TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL CHECK (status IN ('succeeded', 'failed')),
    http_status INTEGER,
    error TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_notification_deliveries_run
    ON notification_deliveries(run_id, id DESC);
