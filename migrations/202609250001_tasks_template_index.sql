-- The template list counts its tasks on read: `TEMPLATE_FIELDS` carries a
-- correlated `COUNT(*)` subquery keyed on `tasks.template_id`. Without an index
-- on that column every template row rescans the task table, and the list is
-- read on every templates-page visit and every open of the create-task dialog.
--
-- No backfill: this is an index only, and the count itself is derived.
CREATE INDEX idx_tasks_template ON tasks(template_id);
