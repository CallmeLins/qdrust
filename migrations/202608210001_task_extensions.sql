-- Per-task execution extensions (timeout / retry / priority / timezone / variables)
ALTER TABLE tasks ADD COLUMN timeout_seconds INTEGER;
ALTER TABLE tasks ADD COLUMN retry_count INTEGER;
ALTER TABLE tasks ADD COLUMN retry_interval_seconds INTEGER;
ALTER TABLE tasks ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tasks ADD COLUMN timezone TEXT;
ALTER TABLE tasks ADD COLUMN variables TEXT;

-- Delayed retry scheduling + retry lineage
ALTER TABLE runs ADD COLUMN run_after INTEGER;
ALTER TABLE runs ADD COLUMN retry_of INTEGER;
CREATE INDEX idx_runs_claim_due ON runs(status, run_after, created_at, id);
CREATE INDEX idx_runs_retry_of ON runs(retry_of);
