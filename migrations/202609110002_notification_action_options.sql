ALTER TABLE notification_actions ADD COLUMN failure_threshold INTEGER NOT NULL DEFAULT 1;
ALTER TABLE notification_actions ADD COLUMN automatic_only INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notification_actions ADD COLUMN title_template TEXT;
ALTER TABLE notification_actions ADD COLUMN body_template TEXT;
ALTER TABLE runs ADD COLUMN trigger TEXT NOT NULL DEFAULT 'scheduled';
