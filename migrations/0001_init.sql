-- Phase 1 foundation: users, servers, tasks.
-- Spec section 8. Keep released migrations immutable.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'viewer'
        CHECK (role IN ('owner', 'admin', 'viewer')),
    is_active INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_login_at TEXT
);

CREATE TABLE IF NOT EXISTS servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    slug TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    runtime_type TEXT NOT NULL DEFAULT 'pumpkin',
    runtime_version TEXT NOT NULL DEFAULT '',
    image_reference TEXT NOT NULL DEFAULT 'ghcr.io/pumpkin-mc/pumpkin:master',
    image_digest TEXT NOT NULL DEFAULT '',
    container_id TEXT NOT NULL DEFAULT '',
    data_path TEXT NOT NULL DEFAULT '',
    game_port INTEGER NOT NULL UNIQUE,
    bind_address TEXT NOT NULL DEFAULT '0.0.0.0',
    desired_state TEXT NOT NULL DEFAULT 'stopped'
        CHECK (desired_state IN ('running', 'stopped')),
    observed_state TEXT NOT NULL DEFAULT 'created',
    created_by TEXT REFERENCES users(id),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    deleted_at TEXT
);

CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    server_id TEXT REFERENCES servers(id) ON DELETE CASCADE,
    requested_by TEXT REFERENCES users(id),
    state TEXT NOT NULL DEFAULT 'queued'
        CHECK (state IN ('queued','running','succeeded','failed','cancel_requested','cancelled')),
    progress_percent INTEGER NOT NULL DEFAULT 0,
    message TEXT NOT NULL DEFAULT '',
    error_code TEXT,
    error_summary TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    started_at TEXT,
    finished_at TEXT,
    cancel_requested_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_tasks_server ON tasks(server_id);
CREATE INDEX IF NOT EXISTS idx_servers_state ON servers(observed_state);
