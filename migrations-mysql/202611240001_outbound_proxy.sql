-- A proxy a task's runs and a subscription's library reads go through, chosen
-- by whoever set them up rather than by the deployment as a whole.
--
-- This is the other half of the template-level `_proxy` variable, which stays
-- what it was: something a template declares for itself, evaluated per step.
-- These columns let an operator name a proxy once for every request of a run
-- (including the ones a template makes) and once for every fetch of a source,
-- which is the case where the template is not theirs to edit — and the only
-- case that covers a task with a plain URL, where no template exists at all.
--
-- Blank means "no proxy", the same as leaving `_proxy` empty.
ALTER TABLE tasks ADD COLUMN proxy VARCHAR(2048);
ALTER TABLE template_subscriptions ADD COLUMN proxy VARCHAR(2048);
