-- Parity with the SQLite 202610080001_notepads migration: QD's notepad table,
-- which backs `api://util/toolbox/notepad`. See that file for what the table is
-- for, why it is not the dropped `notes` table, and why NULL content is kept
-- distinct from ''.
--
-- MEDIUMTEXT rather than TEXT: TEXT caps at 64 KiB and a slot can hold a
-- growing dedup set, not just a cookie. The plugin's own content limit is well
-- under MEDIUMTEXT's 16 MiB, so the column is never the thing that refuses.
--
-- No foreign key, matching every other table here: the MySQL migrations carry
-- no FK constraints, and adding one only for this table would make the two
-- backends differ on delete behaviour for no gain.
CREATE TABLE notepads (
    id BIGINT NOT NULL AUTO_INCREMENT,
    owner_id BIGINT NOT NULL,
    notepad_id BIGINT NOT NULL,
    content MEDIUMTEXT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (id),
    UNIQUE KEY uk_notepads_owner_slot (owner_id, notepad_id),
    KEY idx_notepads_owner_slot (owner_id, notepad_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
