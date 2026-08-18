CREATE TABLE plugins (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 owner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 name TEXT NOT NULL,
 command TEXT NOT NULL,
 config TEXT NOT NULL DEFAULT '{}',
 enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 UNIQUE(owner_id,name)
);
CREATE INDEX idx_plugins_owner ON plugins(owner_id,id);

ALTER TABLE templates ADD COLUMN published INTEGER NOT NULL DEFAULT 0 CHECK(published IN (0,1));
CREATE INDEX idx_templates_public ON templates(published,updated_at DESC) WHERE published=1;
