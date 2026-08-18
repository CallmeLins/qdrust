ALTER TABLE templates ADD COLUMN source_format TEXT NOT NULL DEFAULT 'native_v1';
ALTER TABLE templates ADD COLUMN source TEXT;
