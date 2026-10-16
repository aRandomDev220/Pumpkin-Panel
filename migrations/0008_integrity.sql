-- Audit remediation Task 1: schema integrity.
--
-- 1. Every provenance FK that points at `users(id)` becomes
--    `ON DELETE SET NULL`, so deleting an account never fails on (and never
--    cascades away) the servers/tasks/audit rows it once touched.
-- 2. `servers.name` / `slug` / `game_port` table-level UNIQUEs become
--    *partial* unique indexes scoped to `deleted_at IS NULL`, so a
--    soft-deleted server no longer squats its identity.
-- 3. Indexes the hot query predicates rely on are added.
--
-- SQLite cannot change a foreign key action or drop a table-level UNIQUE in
-- place, so the affected tables are rebuilt with the standard
-- create-new / copy / drop-old / rename procedure.
--
-- sqlx wraps every migration in a transaction, and `PRAGMA foreign_keys` is
-- a no-op while a transaction is pending (SQLite silently ignores it). The
-- migration therefore closes sqlx's implicit transaction, disables foreign
-- key enforcement for the rebuild, runs the rebuild in its own transaction
-- (so a mid-way failure still rolls back), re-enables enforcement, and opens
-- a fresh transaction so sqlx's `_sqlx_migrations` bookkeeping INSERT/COMMIT
-- succeeds. Without the `foreign_keys=OFF` window, `DROP TABLE servers`
-- would perform an implicit DELETE and fire the children's
-- `ON DELETE CASCADE` actions, destroying `tasks`/`backups`/… rows.
--
-- Every statement here is idempotent: a retry after a late failure either
-- finds the already-rebuilt table (and re-copies it) or nothing at all.

COMMIT;

PRAGMA foreign_keys = OFF;

BEGIN;

-- ---------------------------------------------------------------------------
-- servers: provenance FK -> SET NULL; UNIQUE name/slug/game_port -> partial
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS servers_new;
CREATE TABLE servers_new (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    runtime_type TEXT NOT NULL DEFAULT 'pumpkin',
    runtime_version TEXT NOT NULL DEFAULT '',
    image_reference TEXT NOT NULL DEFAULT 'ghcr.io/pumpkin-mc/pumpkin:master',
    image_digest TEXT NOT NULL DEFAULT '',
    container_id TEXT NOT NULL DEFAULT '',
    data_path TEXT NOT NULL DEFAULT '',
    game_port INTEGER NOT NULL,
    bind_address TEXT NOT NULL DEFAULT '0.0.0.0',
    desired_state TEXT NOT NULL DEFAULT 'stopped'
        CHECK (desired_state IN ('running', 'stopped')),
    observed_state TEXT NOT NULL DEFAULT 'created',
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    deleted_at TEXT,
    previous_image_reference TEXT NOT NULL DEFAULT '',
    previous_image_digest TEXT NOT NULL DEFAULT ''
);

INSERT INTO servers_new (
    id, name, slug, description, runtime_type, runtime_version,
    image_reference, image_digest, container_id, data_path, game_port,
    bind_address, desired_state, observed_state, created_by, created_at,
    updated_at, deleted_at, previous_image_reference, previous_image_digest
)
SELECT
    id, name, slug, description, runtime_type, runtime_version,
    image_reference, image_digest, container_id, data_path, game_port,
    bind_address, desired_state, observed_state, created_by, created_at,
    updated_at, deleted_at, previous_image_reference, previous_image_digest
FROM servers;

DROP TABLE servers;
ALTER TABLE servers_new RENAME TO servers;

CREATE UNIQUE INDEX IF NOT EXISTS idx_servers_name_active
    ON servers(name) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_servers_slug_active
    ON servers(slug) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_servers_game_port_active
    ON servers(game_port) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_servers_name ON servers(name);
CREATE INDEX IF NOT EXISTS idx_servers_slug ON servers(slug);
CREATE INDEX IF NOT EXISTS idx_servers_state ON servers(observed_state);

-- ---------------------------------------------------------------------------
-- tasks: requested_by -> SET NULL (+ claim/queue indexes)
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS tasks_new;
CREATE TABLE tasks_new (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    server_id TEXT REFERENCES servers(id) ON DELETE CASCADE,
    requested_by TEXT REFERENCES users(id) ON DELETE SET NULL,
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

INSERT INTO tasks_new (
    id, type, server_id, requested_by, state, progress_percent, message,
    error_code, error_summary, created_at, started_at, finished_at,
    cancel_requested_at
)
SELECT
    id, type, server_id, requested_by, state, progress_percent, message,
    error_code, error_summary, created_at, started_at, finished_at,
    cancel_requested_at
FROM tasks;

DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;

CREATE INDEX IF NOT EXISTS idx_tasks_server ON tasks(server_id);
CREATE INDEX IF NOT EXISTS idx_tasks_server_state ON tasks(server_id, state);
CREATE INDEX IF NOT EXISTS idx_tasks_state_created ON tasks(state, created_at);

-- ---------------------------------------------------------------------------
-- backups: created_by -> SET NULL (+ server/created index)
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS backups_new;
CREATE TABLE backups_new (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    type TEXT NOT NULL DEFAULT 'full',
    state TEXT NOT NULL DEFAULT 'in_progress'
        CHECK (state IN ('in_progress','completed','failed')),
    storage_path TEXT NOT NULL DEFAULT '',
    size_bytes INTEGER NOT NULL DEFAULT 0,
    checksum_algorithm TEXT NOT NULL DEFAULT 'sha256',
    checksum TEXT NOT NULL DEFAULT '',
    consistency_method TEXT NOT NULL DEFAULT 'stopped',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    completed_at TEXT,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    error_summary TEXT,
    protected INTEGER NOT NULL DEFAULT 0
);

INSERT INTO backups_new (
    id, server_id, type, state, storage_path, size_bytes, checksum_algorithm,
    checksum, consistency_method, created_at, completed_at, created_by,
    error_summary, protected
)
SELECT
    id, server_id, type, state, storage_path, size_bytes, checksum_algorithm,
    checksum, consistency_method, created_at, completed_at, created_by,
    error_summary, protected
FROM backups;

DROP TABLE backups;
ALTER TABLE backups_new RENAME TO backups;

CREATE INDEX IF NOT EXISTS idx_backups_server ON backups(server_id);
CREATE INDEX IF NOT EXISTS idx_backups_server_created ON backups(server_id, created_at);

-- ---------------------------------------------------------------------------
-- backup_policies: created_by -> SET NULL
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS backup_policies_new;
CREATE TABLE backup_policies_new (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL DEFAULT '',
    schedule TEXT NOT NULL DEFAULT '',
    timezone TEXT NOT NULL DEFAULT 'UTC',
    retention_count INTEGER NOT NULL DEFAULT 7,
    max_age_days INTEGER NOT NULL DEFAULT 30,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_run_at TEXT,
    next_run_at TEXT,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

INSERT INTO backup_policies_new (
    id, server_id, name, schedule, timezone, retention_count, max_age_days,
    enabled, last_run_at, next_run_at, created_by, created_at, updated_at
)
SELECT
    id, server_id, name, schedule, timezone, retention_count, max_age_days,
    enabled, last_run_at, next_run_at, created_by, created_at, updated_at
FROM backup_policies;

DROP TABLE backup_policies;
ALTER TABLE backup_policies_new RENAME TO backup_policies;

CREATE INDEX IF NOT EXISTS idx_backup_policies_server
    ON backup_policies(server_id);

-- ---------------------------------------------------------------------------
-- scheduled_tasks: created_by -> SET NULL
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS scheduled_tasks_new;
CREATE TABLE scheduled_tasks_new (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('restart')),
    schedule TEXT NOT NULL DEFAULT '',
    timezone TEXT NOT NULL DEFAULT 'UTC',
    enabled INTEGER NOT NULL DEFAULT 1,
    last_run_at TEXT,
    next_run_at TEXT,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

INSERT INTO scheduled_tasks_new (
    id, server_id, kind, schedule, timezone, enabled, last_run_at,
    next_run_at, created_by, created_at, updated_at
)
SELECT
    id, server_id, kind, schedule, timezone, enabled, last_run_at,
    next_run_at, created_by, created_at, updated_at
FROM scheduled_tasks;

DROP TABLE scheduled_tasks;
ALTER TABLE scheduled_tasks_new RENAME TO scheduled_tasks;

CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_server
    ON scheduled_tasks(server_id);

-- ---------------------------------------------------------------------------
-- config_revisions: created_by -> SET NULL (+ server/created index)
-- ---------------------------------------------------------------------------

DROP TABLE IF EXISTS config_revisions_new;
CREATE TABLE config_revisions_new (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    content TEXT NOT NULL,
    hash TEXT NOT NULL,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE(server_id, revision)
);

INSERT INTO config_revisions_new (
    id, server_id, revision, content, hash, created_by, created_at
)
SELECT
    id, server_id, revision, content, hash, created_by, created_at
FROM config_revisions;

DROP TABLE config_revisions;
ALTER TABLE config_revisions_new RENAME TO config_revisions;

CREATE INDEX IF NOT EXISTS idx_config_revisions_server
    ON config_revisions(server_id);
CREATE INDEX IF NOT EXISTS idx_config_revisions_server_created
    ON config_revisions(server_id, created_at);

-- ---------------------------------------------------------------------------
-- Missing predicate indexes (audit_events.actor_user_id already SET NULL;
-- sessions(user_id), metrics_samples(server_id, sampled_at) and
-- audit_events(created_at) indexes already exist from earlier migrations).
-- ---------------------------------------------------------------------------

CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

COMMIT;

PRAGMA foreign_keys = ON;

BEGIN;
