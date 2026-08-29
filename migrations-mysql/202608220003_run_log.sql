-- Parity with the SQLite 202608220003_run_log migration: per-run QD-style log
-- line (the final __log__ value extracted by the template execution).
ALTER TABLE runs ADD COLUMN log MEDIUMTEXT NULL;
