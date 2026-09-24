-- See the SQLite migration of the same name: the template list counts its tasks
-- on read, so the join key needs an index.
CREATE INDEX idx_tasks_template ON tasks(template_id);
