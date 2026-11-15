-- The automatic re-send sweep asks "which failed attempts are still worth
-- another try?" once a minute, and without an index that question reads every
-- delivery ever recorded. `status` leads because it is an equality filter;
-- `created_at` follows so the backoff comparison is answered by the same scan.
CREATE INDEX idx_notification_deliveries_status
    ON notification_deliveries(status, created_at);
