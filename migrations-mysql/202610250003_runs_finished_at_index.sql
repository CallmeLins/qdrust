-- Retention prunes by completion time; see the SQLite sibling for the reasoning.
CREATE INDEX idx_runs_finished_at ON runs (finished_at);
