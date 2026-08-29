-- Per-task random delay (QD-style "当天随机延时区间"): when a scheduled task
-- becomes due, the run is enqueued with run_after = now + rand(0..max_seconds),
-- so the claim worker picks it up only after the jitter elapses.
ALTER TABLE tasks ADD COLUMN random_delay_max_seconds INTEGER NOT NULL DEFAULT 0;
