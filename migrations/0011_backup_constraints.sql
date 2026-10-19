-- A retention count of zero would delete every backup except the protected
-- floor. Enforce the API rule at the database boundary too.
CREATE TRIGGER IF NOT EXISTS backup_policies_retention_insert BEFORE INSERT ON backup_policies WHEN NEW.retention_count < 1 OR NEW.max_age_days < 0 BEGIN SELECT RAISE(ABORT, 'invalid retention policy'); END;
CREATE TRIGGER IF NOT EXISTS backup_policies_retention_update BEFORE UPDATE OF retention_count, max_age_days ON backup_policies WHEN NEW.retention_count < 1 OR NEW.max_age_days < 0 BEGIN SELECT RAISE(ABORT, 'invalid retention policy'); END;
