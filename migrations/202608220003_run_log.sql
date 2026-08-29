-- QD-style per-run log line: the final __log__ value extracted by the last
-- successful template step (rendered in the run history list).
ALTER TABLE runs ADD COLUMN log TEXT;
