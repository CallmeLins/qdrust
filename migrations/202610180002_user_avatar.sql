-- Built-in avatar selection.
--
-- Only a small set of shipped avatar ids is valid; user-supplied files and
-- arbitrary URLs are intentionally unsupported. Existing accounts start on
-- the first avatar so the WebUI always has a concrete selection to render.
ALTER TABLE users ADD COLUMN avatar_id TEXT NOT NULL DEFAULT 'avatar-01';
