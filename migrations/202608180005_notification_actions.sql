CREATE TABLE notification_actions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    channel_id INTEGER NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    event TEXT NOT NULL CHECK (event IN ('success', 'failure', 'always')),
    created_at INTEGER NOT NULL,
    UNIQUE(task_id, channel_id, event)
);
CREATE INDEX idx_notification_actions_task ON notification_actions(task_id, id);
