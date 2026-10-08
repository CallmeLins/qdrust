-- QD's notepad, which backs `api://util/toolbox/notepad`.
--
-- A template stores one blob of text under (account, slot) and reads it back on
-- a later run. That is how a QD template keeps state across runs: the sliding
-- cookie refresh (store the refreshed cookie, read it next time), the page it
-- stopped on, the ids it already handled. Nothing else in the schema offers it
-- -- a task's `variables` are a run-scoped read-only seed.
--
-- Not the same thing as the `notes` table from 202608180003 (dropped in
-- 202608220004): that was a user-facing page with a title per row, and it was
-- removed with the page. This table has no page of its own. It exists because
-- templates address it by URL.
--
-- `content` is nullable and QD distinguishes NULL from '': a slot created but
-- never written appends without the "\r\n" separator, while a slot written with
-- an empty value does not. `updated_at` is this repo's addition, so an operator
-- can tell a live slot from an abandoned one.
--
-- UNIQUE (owner_id, notepad_id) is also this repo's: QD's model declares no
-- such constraint, and its `mod` updates by (owner_id, notepad_id), so a forced
-- duplicate row would leave two rows sharing one slot and both being updated.
-- Here the second insert fails instead.
CREATE TABLE notepads (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    notepad_id INTEGER NOT NULL,
    content TEXT,
    updated_at INTEGER NOT NULL,
    UNIQUE (owner_id, notepad_id)
);

-- Read by (owner_id, notepad_id), which UNIQUE already indexes. The list route
-- reads every slot of one account in slot order.
CREATE INDEX idx_notepads_owner_slot ON notepads(owner_id, notepad_id);
