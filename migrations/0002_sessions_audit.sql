-- Phase 2: sessions, audit, server_settings.
-- Released migrations are immutable; new tables only.

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    revoked_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_sessions_token ON sessions(token_hash);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);

CREATE TABLE IF NOT EXISTS audit_events (
    id TEXT PRIMARY KEY,
    actor_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    target_type TEXT NOT NULL DEFAULT '',
    target_id TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL DEFAULT 'ok',
    request_id TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    metadata TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_audit_time ON audit_events(created_at);
CREATE INDEX IF NOT EXISTS idx_audit_actor ON audit_events(actor_user_id);

CREATE TABLE IF NOT EXISTS server_settings (
    server_id TEXT PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    memory_limit_bytes INTEGER NOT NULL DEFAULT 2147483648,
    cpu_limit REAL NOT NULL DEFAULT 2.0,
    restart_policy TEXT NOT NULL DEFAULT 'no',
    auto_start INTEGER NOT NULL DEFAULT 0,
    backup_policy_id TEXT,
    configuration_revision INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
