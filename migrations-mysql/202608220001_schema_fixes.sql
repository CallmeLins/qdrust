-- Parity with the SQLite migration chain: add the two template indexes that
-- the consolidated initial migration did not carry over. Performance-only.
-- (The SQLite partial unique index idx_runs_active_task is not expressible in
-- MySQL; the one-active-run-per-task invariant is enforced application-side by
-- enqueue_run / schedule_retry.)
CREATE INDEX idx_templates_updated ON templates(updated_at DESC);
CREATE INDEX idx_templates_owner_grp ON templates(owner_id, grp);
