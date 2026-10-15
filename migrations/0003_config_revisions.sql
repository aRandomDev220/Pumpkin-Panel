-- Phase 4: config revision history (pre-change copies for pumpkin.toml saves).
-- Only Task 2 touches migrations/ in this phase; released migrations are immutable.

CREATE TABLE config_revisions (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    content TEXT NOT NULL,
    hash TEXT NOT NULL,
    created_by TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(server_id, revision)
);
CREATE INDEX idx_config_revisions_server ON config_revisions(server_id);
