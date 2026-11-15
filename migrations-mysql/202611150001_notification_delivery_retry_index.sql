-- Retry sweep index; see the SQLite sibling for the reasoning.
CREATE INDEX idx_notification_deliveries_status
    ON notification_deliveries (status, created_at);
