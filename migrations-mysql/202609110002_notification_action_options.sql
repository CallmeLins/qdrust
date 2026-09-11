ALTER TABLE notification_actions ADD COLUMN failure_threshold BIGINT NOT NULL DEFAULT 1;
ALTER TABLE notification_actions ADD COLUMN automatic_only TINYINT(1) NOT NULL DEFAULT 0;
ALTER TABLE notification_actions ADD COLUMN title_template TEXT NULL;
ALTER TABLE notification_actions ADD COLUMN body_template TEXT NULL;
ALTER TABLE runs ADD COLUMN `trigger` VARCHAR(16) NOT NULL DEFAULT 'scheduled';
