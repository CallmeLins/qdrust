-- Parity with the SQLite 202608220002_random_delay migration: per-task random
-- delay (QD-style "当天随机延时区间"). Due runs are enqueued with
-- run_after = now + rand(0..max_seconds).
ALTER TABLE tasks ADD COLUMN random_delay_max_seconds BIGINT NOT NULL DEFAULT 0;
