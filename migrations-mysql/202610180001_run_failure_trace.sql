-- Keep the latest full failure trace separate from the short user-facing
-- `runs.log` line rendered in the run table.
ALTER TABLE runs ADD COLUMN failure_trace MEDIUMTEXT NULL;
