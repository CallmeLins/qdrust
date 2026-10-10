-- Per-account default notification templates (issue #45).
--
-- A notification's title and body live on the task<->channel binding, so an
-- account with 14 tasks and 2 channels keeps 28 copies of the same text and has
-- to PUT all 28 to change one word. This table gives those templates a home
-- outside any single binding. The order is: the binding's own template, else the
-- owner's default for the event, else the built-in one the scheduler carries.
-- An empty column means "not set", never "deliver nothing".
--
-- One row per (account, event) rather than one row with four columns: the pair
-- of templates that belongs to `success` stays together, and a third event would
-- be a row instead of another migration.
--
-- This cannot be modelled as a task-less row in `notification_actions`: that
-- table's `task_id` is NOT NULL and its unique key is (task_id, channel_id,
-- event), so two account-wide rows for one channel and event would collide.
-- Nothing here names a channel either -- a default is about wording, not about
-- where the message goes.
CREATE TABLE notification_default_templates (
    owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event TEXT NOT NULL CHECK (event IN ('success', 'failure')),
    title_template TEXT,
    body_template TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (owner_id, event)
);
