-- Audit remediation Task 2: persistent application settings.
--
-- `app_settings` backs one-shot setup (`setup_complete`) so first-run
-- provisioning cannot be repeated even if the `users` table is later
-- emptied. It is a generic key/value store per spec section 8.11. New
-- tables only; released migrations are immutable.

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
