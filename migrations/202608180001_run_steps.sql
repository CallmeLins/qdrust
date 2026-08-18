CREATE TABLE run_steps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    step_index INTEGER NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('succeeded', 'failed')),
    http_status INTEGER,
    body_size INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL
);

CREATE UNIQUE INDEX idx_run_steps_order ON run_steps(run_id, step_index);
