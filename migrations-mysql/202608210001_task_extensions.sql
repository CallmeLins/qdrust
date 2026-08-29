-- Per-task execution extensions (timeout / retry / priority / timezone / variables)
ALTER TABLE tasks ADD COLUMN timeout_seconds INT NULL;
ALTER TABLE tasks ADD COLUMN retry_count INT NULL;
ALTER TABLE tasks ADD COLUMN retry_interval_seconds INT NULL;
ALTER TABLE tasks ADD COLUMN priority INT NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN timezone VARCHAR(64) NULL;
ALTER TABLE tasks ADD COLUMN variables TEXT NULL;

-- Delayed retry scheduling + retry lineage
ALTER TABLE runs ADD COLUMN run_after BIGINT NULL;
ALTER TABLE runs ADD COLUMN retry_of BIGINT NULL;
CREATE INDEX idx_runs_claim_due ON runs(status, run_after, created_at, id);
CREATE INDEX idx_runs_retry_of ON runs(retry_of);
