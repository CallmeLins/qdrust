-- Parity with the SQLite 202610100002_notification_default_templates
-- migration: per-account default notification templates (issue #45). See that
-- file for the delivery order, why it is one row per event, and why this cannot
-- be a task-less row in `notification_actions`.
--
-- MEDIUMTEXT for the body: it carries the run log, which a QD template writes
-- as free-form text and can be long. The title is a single line.
--
-- No foreign key, matching every other table here: the MySQL migrations carry no
-- FK constraints, and adding one only for this table would make the two backends
-- differ on delete behaviour for no gain.
CREATE TABLE notification_default_templates (
    owner_id BIGINT NOT NULL,
    event VARCHAR(16) NOT NULL,
    title_template VARCHAR(255) NULL,
    body_template MEDIUMTEXT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (owner_id, event)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
