-- The notes feature (standalone page) has been removed from the product; drop
-- its table. Historical migration 202608180003_notes.sql stays applied for
-- checksum integrity.
DROP TABLE IF EXISTS notes;
