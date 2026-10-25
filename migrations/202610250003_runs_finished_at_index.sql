-- Retention prunes by completion time (`runs.finished_at`). Without an index the
-- hourly sweep scans every run ever recorded to find the handful that are old
-- enough to delete, so the sweep gets slower for as long as the deployment lives.
CREATE INDEX idx_runs_finished_at ON runs(finished_at);
