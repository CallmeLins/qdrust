-- Delivery ledger; see the SQLite sibling for the reasoning. MySQL carries no
-- foreign keys in this project (the inline REFERENCES are parsed and ignored),
-- so the cascade is enforced by the delete paths in `store.rs`.
CREATE TABLE notification_deliveries (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    run_id BIGINT NOT NULL,
    task_id BIGINT NOT NULL,
    action_id BIGINT,
    channel_id BIGINT,
    channel_kind VARCHAR(32) NOT NULL,
    event VARCHAR(16) NOT NULL,
    attempt BIGINT NOT NULL DEFAULT 1,
    status VARCHAR(16) NOT NULL,
    http_status BIGINT,
    error TEXT,
    created_at BIGINT NOT NULL,
    INDEX idx_notification_deliveries_run (run_id, id DESC)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
