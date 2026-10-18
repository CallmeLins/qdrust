-- Full diagnostic trace for the latest failed run of each task.
-- Kept separate from `runs.log`: the log column is the short QD-style
-- user-facing line rendered in the run table, while this column is the
-- click-to-open troubleshooting payload.
ALTER TABLE runs ADD COLUMN failure_trace TEXT;
