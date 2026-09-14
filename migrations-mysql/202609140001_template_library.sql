ALTER TABLE template_subscriptions ADD COLUMN mode VARCHAR(16) NOT NULL DEFAULT 'all';

CREATE TABLE template_imports (
    id BIGINT SIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    subscription_id BIGINT NOT NULL REFERENCES template_subscriptions(id) ON DELETE CASCADE,
    template_id BIGINT NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
    entry_name VARCHAR(255) NOT NULL,
    entry_version VARCHAR(32),
    source_url TEXT,
    imported_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    UNIQUE KEY uk_template_imports_entry (subscription_id, entry_name),
    INDEX idx_template_imports_template (template_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
