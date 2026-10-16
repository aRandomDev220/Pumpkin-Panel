-- Phase 7: image update rollback target (Task 1).
-- `previous_image_*` persists across successful updates; only the next
-- update overwrites it, so rollback always has a target.

ALTER TABLE servers ADD COLUMN previous_image_reference TEXT NOT NULL DEFAULT '';
ALTER TABLE servers ADD COLUMN previous_image_digest TEXT NOT NULL DEFAULT '';
