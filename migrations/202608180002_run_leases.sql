DROP INDEX idx_run_steps_order;
DROP INDEX idx_runs_task_started;

ALTER TABLE run_steps RENAME TO run_steps_legacy;
ALTER TABLE runs RENAME TO runs_legacy;

CREATE TABLE runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id INTEGER NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('pending', 'leased', 'running', 'succeeded', 'failed', 'cancelled')),
    http_status INTEGER,
    error TEXT,
    started_at INTEGER,
    finished_at INTEGER,
    created_at INTEGER NOT NULL,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    attempt INTEGER NOT NULL DEFAULT 0,
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1))
);

INSERT INTO runs(id,task_id,status,http_status,error,started_at,finished_at,created_at,attempt)
SELECT id,task_id,status,http_status,error,started_at,finished_at,started_at,1
FROM runs_legacy;

CREATE INDEX idx_runs_task_created ON runs(task_id, created_at DESC, id DESC);
CREATE INDEX idx_runs_claim ON runs(status, lease_expires_at, created_at, id);
CREATE UNIQUE INDEX idx_runs_active_task ON runs(task_id)
WHERE status IN ('pending', 'leased', 'running');

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

INSERT INTO run_steps(id,run_id,step_index,name,status,http_status,body_size,error,started_at,finished_at)
SELECT id,run_id,step_index,name,status,http_status,body_size,error,started_at,finished_at
FROM run_steps_legacy;

CREATE UNIQUE INDEX idx_run_steps_order ON run_steps(run_id, step_index);

DROP TABLE run_steps_legacy;
DROP TABLE runs_legacy;
