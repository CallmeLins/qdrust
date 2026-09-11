-- MySQL channel kinds are intentionally not constrained by a CHECK in the
-- consolidated schema; validation is performed by the server store layer.
-- This migration exists to keep the MySQL migration chain in parity with the
-- SQLite channel-kind expansion.
SELECT 1;
