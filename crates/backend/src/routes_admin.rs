//! Admin surface (Phase 2): task inspection, user management, audit
//! trail, and on-demand reconcile. All routes sit behind auth +
//! fine-grained permissions at router wiring time.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::{audit, auth::AuthUser, runtime, task_runner, AppState};

fn error(status: StatusCode, code: &'static str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error_code": code,
            "message": message,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

/// GET /api/v1/tasks/:id — task state for polling UIs. `kind` aliases
/// `type` for the frontend contract; both are returned.
pub async fn get_task(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    match task_runner::get(&state.pool, &id).await {
        Ok(Some(t)) => Json(serde_json::json!({
            "id": t.id,
            "kind": t.task_type,
            "type": t.task_type,
            "state": t.state.as_str(),
            "server_id": t.server_id,
            "requested_by": t.requested_by,
            "progress_percent": t.progress_percent,
            "message": t.message,
            "error": t.error_summary,
            "error_code": t.error_code,
            "error_summary": t.error_summary,
        }))
        .into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "TASK_NOT_FOUND", "no such task"),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to load task",
        ),
    }
}

/// POST /api/v1/tasks/:id/cancel — cooperative cancellation. Only `queued`
/// tasks can be withdrawn; a `running` Docker op is never abandoned
/// mid-write (409 with an explanation).
pub async fn cancel_task(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    let request_id = audit::request_id();
    let task = match task_runner::get(&state.pool, &id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return error(StatusCode::NOT_FOUND, "TASK_NOT_FOUND", "no such task");
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load task",
            )
        }
    };
    if !matches!(
        task.state,
        task_runner::TaskState::Queued | task_runner::TaskState::CancelRequested
    ) {
        return error(
            StatusCode::CONFLICT,
            "TASK_CANCEL_UNSAFE",
            "task is already running; cancellation is only safe before dispatch",
        );
    }
    let updated = sqlx::query(
        "UPDATE tasks SET state = 'cancelled', finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ? AND state IN ('queued', 'cancel_requested')",
    )
    .bind(&id)
    .execute(&state.pool)
    .await;
    match updated {
        Ok(r) if r.rows_affected() == 1 => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "task.cancel",
                "task",
                &id,
                "ok",
                &request_id,
                serde_json::json!({}),
            )
            .await;
            Json(serde_json::json!({ "id": id, "state": "cancelled" })).into_response()
        }
        _ => error(
            StatusCode::CONFLICT,
            "TASK_STATE_CHANGED",
            "task already claimed",
        ),
    }
}

// ---------------------------------------------------------------------------
// Users (owner-only: `users.manage`)
// ---------------------------------------------------------------------------

/// GET /api/v1/users — no password hashes ever leave the server.
pub async fn list_users(State(state): State<AppState>, _user: AuthUser) -> Response {
    let rows = sqlx::query(
        "SELECT id, username, role, is_active, created_at, updated_at, last_login_at
         FROM users ORDER BY created_at ASC",
    )
    .fetch_all(&state.pool)
    .await;
    match rows {
        Ok(rows) => {
            use sqlx::Row as _;
            let users: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.get::<String, _>("id"),
                        "username": r.get::<String, _>("username"),
                        "role": r.get::<String, _>("role"),
                        "is_active": r.get::<i64, _>("is_active") != 0,
                        "created_at": r.get::<String, _>("created_at"),
                        "updated_at": r.get::<String, _>("updated_at"),
                        "last_login_at": r.get::<Option<String>, _>("last_login_at"),
                    })
                })
                .collect();
            Json(serde_json::json!({ "users": users })).into_response()
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to list users",
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub role: Option<String>,
    pub is_active: Option<bool>,
}

/// PATCH /api/v1/users/:id — role / activation. Refuses to remove the
/// last active owner (would brick first-run recovery).
pub async fn update_user(
    State(state): State<AppState>,
    actor: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> Response {
    let request_id = audit::request_id();
    if body.role.is_none() && body.is_active.is_none() {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_CONFIGURATION",
            "nothing to update",
        );
    }
    if let Some(role) = &body.role {
        if crate::auth::Role::from_str(role).is_none() {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_CONFIGURATION",
                "role must be owner, admin, or viewer",
            );
        }
    }
    let target: Option<(String, i64)> =
        sqlx::query_as("SELECT role, is_active FROM users WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
    let Some((target_role, _)) = target else {
        return error(StatusCode::NOT_FOUND, "USER_NOT_FOUND", "no such user");
    };
    // Last-owner guard: any change that would deactivate or demote an
    // owner requires another active owner to remain.
    let demotes_owner = target_role == "owner"
        && (body.role.as_deref().is_some_and(|r| r != "owner") || body.is_active == Some(false));
    if demotes_owner {
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE role = 'owner' AND is_active = 1 AND id != ?",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
        if others == 0 {
            return error(
                StatusCode::CONFLICT,
                "LAST_OWNER",
                "cannot demote or deactivate the last active owner",
            );
        }
    }
    if let Some(role) = &body.role {
        let _ = sqlx::query("UPDATE users SET role = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
            .bind(role)
            .bind(&id)
            .execute(&state.pool)
            .await;
    }
    if let Some(active) = body.is_active {
        let _ = sqlx::query("UPDATE users SET is_active = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
            .bind(if active { 1 } else { 0 })
            .bind(&id)
            .execute(&state.pool)
            .await;
        if !active {
            // Deactivation revokes every session immediately.
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let _ = sqlx::query(
                "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
            )
            .bind(&now)
            .bind(&id)
            .execute(&state.pool)
            .await;
        }
    }
    audit::record(
        &state.pool,
        Some(&actor.id),
        "users.update",
        "user",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "role": body.role, "is_active": body.is_active }),
    )
    .await;
    Json(serde_json::json!({ "id": id, "ok": true })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: String,
}

/// POST /api/v1/users — create a user (controller applies `UsersManage`).
/// Empty usernames, passwords under 8 chars, and unknown roles are 422;
/// duplicate usernames are 409. The DB UNIQUE constraint is the backstop
/// for a first-user-style creation race (setup path untouched).
#[allow(dead_code)] // controller wires the route in main.rs
pub async fn create_user(
    State(state): State<AppState>,
    actor: AuthUser,
    Json(body): Json<CreateUserRequest>,
) -> Response {
    let request_id = audit::request_id();
    let username = body.username.trim().to_owned();
    if username.is_empty() {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_USERNAME",
            "username must not be empty",
        );
    }
    if body.password.len() < 8 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "WEAK_PASSWORD",
            "password must be at least 8 characters",
        );
    }
    let Some(role) = crate::auth::Role::from_str(body.role.trim()) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_ROLE",
            "role must be owner, admin, or viewer",
        );
    };
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
        .bind(&username)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    if exists > 0 {
        return error(
            StatusCode::CONFLICT,
            "USER_EXISTS",
            "username already exists",
        );
    }
    let password_hash = match crate::auth::hash_password(&body.password) {
        Ok(h) => h,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to hash password",
            )
        }
    };
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let inserted = sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, is_active, created_at, updated_at)
         VALUES (?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&username)
    .bind(&password_hash)
    .bind(role.as_str())
    .bind(&now)
    .bind(&now)
    .execute(&state.pool)
    .await;
    if let Err(e) = inserted {
        if e.to_string().contains("UNIQUE") {
            return error(
                StatusCode::CONFLICT,
                "USER_EXISTS",
                "username already exists",
            );
        }
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to create user",
        );
    }
    audit::record(
        &state.pool,
        Some(&actor.id),
        "users.create",
        "user",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "username": username, "role": role.as_str() }),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "username": username, "role": role.as_str() })),
    )
        .into_response()
}

/// DELETE /api/v1/users/:id — delete a user (controller applies
/// `UsersManage`). Refuses to delete the final active owner (409
/// `LAST_OWNER`, same guard as the update path) and revokes every session
/// immediately. Self-delete is allowed: the caller's token stops working,
/// so the client must discard it after the 204.
#[allow(dead_code)] // controller wires the route in main.rs
pub async fn delete_user(
    State(state): State<AppState>,
    actor: AuthUser,
    Path(id): Path<String>,
) -> Response {
    let request_id = audit::request_id();
    let target: Option<(String, i64)> =
        sqlx::query_as("SELECT role, is_active FROM users WHERE id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
    let Some((target_role, target_active)) = target else {
        return error(StatusCode::NOT_FOUND, "USER_NOT_FOUND", "no such user");
    };
    if target_role == "owner" && target_active != 0 {
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE role = 'owner' AND is_active = 1 AND id != ?",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
        if others == 0 {
            return error(
                StatusCode::CONFLICT,
                "LAST_OWNER",
                "cannot delete the last active owner",
            );
        }
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let _ =
        sqlx::query("UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL")
            .bind(&now)
            .bind(&id)
            .execute(&state.pool)
            .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await;
    audit::record(
        &state.pool,
        Some(&actor.id),
        "users.delete",
        "user",
        &id,
        "ok",
        &request_id,
        serde_json::json!({}),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    #[serde(default = "default_audit_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub action: Option<String>,
}

fn default_audit_limit() -> i64 {
    50
}

/// GET /api/v1/audit — newest first, capped. Metadata is already redacted
/// at write time (see `audit::record`).
pub async fn list_audit(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(query): Query<AuditQuery>,
) -> Response {
    let limit = query.limit.clamp(1, 200);
    let offset = query.offset.max(0);
    let rows = if let Some(action) = query.action.filter(|a| !a.is_empty()) {
        sqlx::query(
            "SELECT id, actor_user_id, action, target_type, target_id, outcome,
                    request_id, created_at, metadata
             FROM audit_events WHERE action = ?
             ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(action)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await
    } else {
        sqlx::query(
            "SELECT id, actor_user_id, action, target_type, target_id, outcome,
                    request_id, created_at, metadata
             FROM audit_events
             ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await
    };
    match rows {
        Ok(rows) => {
            use sqlx::Row as _;
            let events: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.get::<String, _>("id"),
                        "actor_user_id": r.get::<Option<String>, _>("actor_user_id"),
                        "action": r.get::<String, _>("action"),
                        "target_type": r.get::<String, _>("target_type"),
                        "target_id": r.get::<String, _>("target_id"),
                        "outcome": r.get::<String, _>("outcome"),
                        "request_id": r.get::<String, _>("request_id"),
                        "created_at": r.get::<String, _>("created_at"),
                        "metadata": r.get::<String, _>("metadata"),
                    })
                })
                .collect();
            Json(serde_json::json!({ "events": events })).into_response()
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to list audit events",
        ),
    }
}

// ---------------------------------------------------------------------------
// Diagnostics export
// ---------------------------------------------------------------------------

/// GET /api/v1/admin/diagnostics — redacted operator bundle for support
/// tickets (controller applies `SettingsManage`).
///
/// Assembled on demand from live, non-secret facts only:
/// `{version, os, arch, schema_version, docker, servers, capabilities,
/// recent_failures, config_validation}`. This handler NEVER reads the
/// `users`, `sessions`, or `server_settings` tables, never dumps the
/// environment, and never includes file contents or data paths — server
/// entries carry only `{id, name, desired_state, observed_state}` and
/// failures carry only `{id, type, server_id, message}` (error summaries,
/// never bodies).
#[allow(dead_code)] // controller wires the route in main.rs; drop then
pub async fn export_diagnostics(State(state): State<AppState>, _user: AuthUser) -> Response {
    let schema_version: Option<i64> =
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(None);

    let server_rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT id, name, desired_state, observed_state, data_path
          FROM servers WHERE deleted_at IS NULL ORDER BY name ASC",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let failure_rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, type, server_id, COALESCE(error_summary, message)
          FROM tasks WHERE state = 'failed' ORDER BY rowid DESC LIMIT 20",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let mut servers = Vec::with_capacity(server_rows.len());
    let mut config_validation = Vec::with_capacity(server_rows.len());
    for (id, name, desired_state, observed_state, data_path) in &server_rows {
        servers.push(serde_json::json!({
            "id": id,
            "name": name,
            "desired_state": desired_state,
            "observed_state": observed_state,
        }));
        // Validate on-disk content when present. An absent pumpkin.toml is
        // "unconfigured", not "invalid" — fresh servers have none yet — so a
        // missing file validates clean with no errors.
        let entry =
            match std::fs::read_to_string(std::path::Path::new(data_path).join("pumpkin.toml")) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    serde_json::json!({ "server_id": id, "ok": true, "errors": [] })
                }
                Err(e) => serde_json::json!({
                    "server_id": id, "ok": false, "errors": [format!("cannot read config: {e}")]
                }),
                Ok(content) => match crate::config_profile::parse_and_validate(&content) {
                    Ok(_) => serde_json::json!({ "server_id": id, "ok": true, "errors": [] }),
                    Err(errors) => serde_json::json!({
                        "server_id": id, "ok": false, "errors": errors
                    }),
                },
            };
        config_validation.push(entry);
    }

    let recent_failures: Vec<serde_json::Value> = failure_rows
        .into_iter()
        .map(|(id, kind, server_id, message)| {
            serde_json::json!({
                "id": id,
                "type": kind,
                "server_id": server_id,
                "message": message.unwrap_or_default(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "schema_version": schema_version,
        "docker": { "reachable": state.adapter.is_some() },
        "servers": servers,
        "capabilities": capabilities_profile(),
        "recent_failures": recent_failures,
        "config_validation": config_validation,
    }))
    .into_response()
}

/// Static capability profile, same shape as
/// `routes_servers::server_capabilities` (read-only reference — the literal
/// is duplicated here because that module is owned by another workstream).
/// Values are the only ones the UI may use to gate features.
fn capabilities_profile() -> serde_json::Value {
    serde_json::json!({
        "runtime": "pumpkin",
        "profile_version": 1,
        "capabilities": {
            "stdout_logs": {"status": "verified", "detail": "docker logs + logs/latest.log"},
            "minecraft_status_ping": {"status": "verified", "detail": "Java status ping on 25565"},
            "toml_config": {"status": "verified", "detail": "pumpkin.toml round-trips"},
            "graceful_shutdown": {"status": "verified-with-limitations", "detail": "SIGTERM ~1s observed on tested build; per-version timeout TBD"},
            "player_count": {"status": "verified-with-limitations", "detail": "status-ping sample only, no separate API"},
            "minecraft_query": {"status": "unknown", "detail": "config present, not exercised"},
            "remote_console": {"status": "unknown", "detail": "stdin console not yet tested; input stays read-only"},
            "rcon": {"status": "unknown", "detail": "documented but absent from generated config; do not use"},
            "health_endpoint": {"status": "not-supported", "detail": "image has TCP check only"}
        }
    })
}

/// POST /api/v1/admin/reconcile — read-only drift report between the
/// `servers` table and managed Docker containers. Never deletes anything.
pub async fn trigger_reconcile(State(state): State<AppState>, user: AuthUser) -> Response {
    let Some(adapter) = &state.adapter else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "RUNTIME_UNAVAILABLE",
            "no runtime attached",
        );
    };
    match runtime::reconcile(&state.pool, adapter.as_ref()).await {
        Ok(items) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "admin.reconcile",
                "runtime",
                "docker",
                "ok",
                &audit::request_id(),
                serde_json::json!({ "discrepancies": items.len() }),
            )
            .await;
            Json(serde_json::json!({ "discrepancies": items })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error_code": e.code(),
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Manager database backup / staged restore (controller: `SettingsManage`)
// ---------------------------------------------------------------------------

/// Highest `user_version` this manager understands. Databases created by
/// sqlx migrations leave `user_version` at 0; anything higher comes from
/// an unknown newer manager and must never be staged.
const MAX_SUPPORTED_DB_USER_VERSION: i64 = 0;

/// SQLite file magic (`SQLite format 3\0`).
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// Path the controller's boot swap watches: `<live>.pending-restore`.
/// `restore_database` stages validated bytes here; the live DB is never
/// swapped while the manager is running. Keep this signature stable.
pub fn pending_restore_path(db_path: &std::path::Path) -> std::path::PathBuf {
    let mut pending = db_path.as_os_str().to_owned();
    pending.push(".pending-restore");
    std::path::PathBuf::from(pending)
}

/// Filesystem path of the live manager database behind the pool
/// (`PRAGMA database_list`, `main` entry). `None` for non-file
/// databases (e.g. in-memory pools).
async fn live_db_path(pool: &sqlx::SqlitePool) -> Option<std::path::PathBuf> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as("PRAGMA database_list")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    rows.into_iter().find_map(|(_, name, file)| {
        if name == "main" && !file.is_empty() {
            Some(std::path::PathBuf::from(file))
        } else {
            None
        }
    })
}

fn utc_stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Escape a path for interpolation into `VACUUM INTO '<path>'`
/// (SQLite accepts no bound parameter there).
fn vacuum_into_sql(path: &std::path::Path) -> String {
    format!(
        "VACUUM INTO '{}'",
        path.to_string_lossy().replace('\'', "''")
    )
}

/// GET /api/v1/admin/database/backup — consistent snapshot of the live
/// manager DB as an attachment download
/// (`pumpkin-manager-<schema>-<ts>.db`). Uses `VACUUM INTO` a temp file
/// so a WAL-mode live database streams consistently.
#[allow(dead_code)] // controller wires the route in main.rs
pub async fn backup_database(State(state): State<AppState>, user: AuthUser) -> Response {
    if live_db_path(&state.pool).await.is_none() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "manager database is not file-backed",
        );
    };
    let schema: String =
        sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .flatten()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "0".to_string());
    let snapshot =
        std::env::temp_dir().join(format!("pm-manager-backup-{}.db", uuid::Uuid::new_v4()));
    if sqlx::query(&vacuum_into_sql(&snapshot))
        .execute(&state.pool)
        .await
        .is_err()
    {
        let _ = std::fs::remove_file(&snapshot);
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to snapshot database",
        );
    }
    let bytes = match std::fs::read(&snapshot) {
        Ok(b) => b,
        Err(_) => {
            let _ = std::fs::remove_file(&snapshot);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to read snapshot",
            );
        }
    };
    let _ = std::fs::remove_file(&snapshot);
    let filename = format!("pumpkin-manager-{schema}-{}.db", utc_stamp());
    audit::record(
        &state.pool,
        Some(&user.id),
        "admin.database.backup",
        "database",
        "manager",
        "ok",
        &audit::request_id(),
        serde_json::json!({ "file": filename, "bytes": bytes.len() }),
    )
    .await;
    (
        [
            ("content-type", "application/x-sqlite3".to_string()),
            (
                "content-disposition",
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// POST /api/v1/admin/database/restore — validate an uploaded manager DB
/// and STAGE it for the next boot. Never swaps the live file: the current
/// DB is copied aside as `<live>.pre-restore-<ts>` and the upload is
/// written to `pending_restore_path(db_path)`, which the controller
/// applies at startup. Returns `200 {staged, restart_required}`.
#[allow(dead_code)] // controller wires the route in main.rs
pub async fn restore_database(
    State(state): State<AppState>,
    user: AuthUser,
    mut multipart: Multipart,
) -> Response {
    let mut data: Option<Vec<u8>> = None;
    let mut total: u64 = 0;
    loop {
        let field = match multipart.next_field().await {
            Ok(f) => f,
            Err(_) => {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "INVALID_DATABASE_FILE",
                    "invalid multipart body",
                )
            }
        };
        let Some(field) = field else { break };
        let bytes = match field.bytes().await {
            Ok(b) => b,
            Err(_) => {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "INVALID_DATABASE_FILE",
                    "failed to read upload",
                )
            }
        };
        total += bytes.len() as u64;
        if total > crate::file_service::MAX_UPLOAD_BYTES {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "DATABASE_TOO_LARGE",
                "database file too large",
            );
        }
        if data.is_none() {
            data = Some(bytes.to_vec());
        }
    }
    let Some(data) = data.filter(|d| !d.is_empty()) else {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_DATABASE_FILE",
            "no file uploaded",
        );
    };
    if data.len() < 100 || !data.starts_with(SQLITE_MAGIC) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_DATABASE_FILE",
            "not a SQLite database file",
        );
    }
    // Validate PRAGMAs + required tables against a temp copy, so a bad
    // upload never touches the live DB.
    let scratch =
        std::env::temp_dir().join(format!("pm-manager-restore-{}.db", uuid::Uuid::new_v4()));
    if std::fs::write(&scratch, &data).is_err() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to stage upload",
        );
    }
    if let Some(resp) = validate_restore_file(&scratch).await {
        let _ = std::fs::remove_file(&scratch);
        return resp;
    }
    let _ = std::fs::remove_file(&scratch);
    let Some(db_path) = live_db_path(&state.pool).await else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "manager database is not file-backed",
        );
    };
    // Copy the current DB aside before staging (consistent snapshot via
    // the same `VACUUM INTO` technique as the backup download; plain
    // file copy as fallback).
    let aside =
        std::path::PathBuf::from(format!("{}.pre-restore-{}", db_path.display(), utc_stamp()));
    let aside_ok = sqlx::query(&vacuum_into_sql(&aside))
        .execute(&state.pool)
        .await
        .is_ok()
        || std::fs::copy(&db_path, &aside).is_ok();
    if !aside_ok {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to preserve current database",
        );
    }
    let pending = pending_restore_path(&db_path);
    if std::fs::write(&pending, &data).is_err() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to stage restore",
        );
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "admin.database.restore_staged",
        "database",
        "manager",
        "ok",
        &audit::request_id(),
        serde_json::json!({ "pre_restore": aside.display().to_string(), "bytes": data.len() }),
    )
    .await;
    Json(serde_json::json!({ "staged": true, "restart_required": true })).into_response()
}

/// Validate a candidate restore file, read-only: parseable SQLite,
/// `user_version` within the supported range, and the required
/// `users`/`servers` tables present. Returns the rejection response, or
/// `None` when the file is acceptable.
async fn validate_restore_file(path: &std::path::Path) -> Option<Response> {
    let opts = SqliteConnectOptions::new().filename(path).read_only(true);
    let pool = match SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
    {
        Ok(p) => p,
        Err(_) => {
            return Some(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_DATABASE_FILE",
                "not a readable SQLite database",
            ))
        }
    };
    let version: i64 = match sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
    {
        Ok(v) => v,
        Err(_) => {
            pool.close().await;
            return Some(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_DATABASE_FILE",
                "not a readable SQLite database",
            ));
        }
    };
    if version < 0 || version > MAX_SUPPORTED_DB_USER_VERSION {
        pool.close().await;
        return Some(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "UNSUPPORTED_SCHEMA",
            &format!("unsupported database schema version {version}"),
        ));
    }
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('users', 'servers')",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    pool.close().await;
    if !(tables.iter().any(|t| t == "users") && tables.iter().any(|t| t == "servers")) {
        return Some(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "UNSUPPORTED_SCHEMA",
            "not a pumpkin-manager database (missing users/servers tables)",
        ));
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::auth::Role;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for file in [
            include_str!("../../../migrations/0001_init.sql"),
            include_str!("../../../migrations/0002_sessions_audit.sql"),
        ] {
            for stmt in file.split(";\n") {
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                if !stmt.to_uppercase().contains("CREATE") && !stmt.to_uppercase().contains("INDEX")
                {
                    continue;
                }
                sqlx::query(stmt).execute(&pool).await.unwrap();
            }
        }
        pool
    }

    fn owner(id: &str) -> AuthUser {
        AuthUser {
            id: id.to_string(),
            username: "owner".to_string(),
            role: Role::Owner,
        }
    }

    async fn body_text(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Secrets planted in the DB (password hash, session token hash) must
    /// never appear in the diagnostics bundle, which must still carry the
    /// version and recent failures.
    #[tokio::test]
    async fn diagnostics_has_no_secrets() {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pm-diag-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let data_dir = root.join("srv-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let state = AppState::for_tests(pool.clone(), root);

        let hash_secret = "phant-hash-SECRET-abc123";
        let token_secret = "phant-token-SECRET-xyz789";
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role) VALUES (?, 'owner', ?, 'owner')",
        )
        .bind("u-owner")
        .bind(hash_secret)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, user_id, token_hash, expires_at) VALUES (?, 'u-owner', ?, '2099-01-01T00:00:00.000Z')",
        )
        .bind("sess-1")
        .bind(token_secret)
        .execute(&pool)
        .await
        .unwrap();
        let server_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO servers (id, name, slug, game_port, data_path, desired_state, observed_state)
             VALUES (?, 'diag1', 'diag1', 25631, ?, 'stopped', 'created')",
        )
        .bind(&server_id)
        .bind(data_dir.to_string_lossy().to_string())
        .execute(&pool)
        .await
        .unwrap();
        // Broken on-disk config must surface as a validation failure.
        std::fs::write(data_dir.join("pumpkin.toml"), "[[[unclosed").unwrap();
        sqlx::query(
            "INSERT INTO tasks (id, type, server_id, state, error_code, error_summary)
             VALUES (?, 'restart', ?, 'failed', 'RUNTIME_UNAVAILABLE', 'boom-failure-marker')",
        )
        .bind("task-failed-1")
        .bind(&server_id)
        .execute(&pool)
        .await
        .unwrap();

        let text = body_text(export_diagnostics(State(state), owner("u-owner")).await).await;
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert!(!text.contains(hash_secret), "password hash leaked");
        assert!(!text.contains(token_secret), "session token leaked");
        assert!(!text.contains("phant-hash"), "password hash leaked");
        assert!(!text.contains("phant-token"), "session token leaked");
        assert_eq!(
            body["version"],
            serde_json::json!(env!("CARGO_PKG_VERSION"))
        );
        assert!(body["os"].is_string());
        assert!(body["arch"].is_string());
        assert_eq!(body["docker"]["reachable"], serde_json::json!(false));
        assert!(
            text.contains("boom-failure-marker"),
            "recent failures missing"
        );
        assert_eq!(body["servers"][0]["name"], serde_json::json!("diag1"));
        assert!(
            body["servers"][0].get("data_path").is_none(),
            "data path leaked"
        );
        assert_eq!(
            body["capabilities"]["runtime"],
            serde_json::json!("pumpkin")
        );
        let validation = body["config_validation"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["server_id"] == serde_json::json!(server_id))
            .expect("config_validation entry missing");
        assert_eq!(validation["ok"], serde_json::json!(false));
        assert!(!validation["errors"].as_array().unwrap().is_empty());
    }

    // --- Task 2 (Phase 7): user admin + manager DB recovery ---

    use axum_test::TestServer;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn file_backed_pool(dir: &std::path::Path) -> sqlx::SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(dir.join("pumpkin-manager.db"))
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        for file in [
            include_str!("../../../migrations/0001_init.sql"),
            include_str!("../../../migrations/0002_sessions_audit.sql"),
        ] {
            let stripped: String = file
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            for stmt in stripped.split(';') {
                let stmt = stmt.trim();
                if stmt.is_empty() || stmt.to_uppercase().starts_with("PRAGMA") {
                    continue;
                }
                sqlx::query(stmt).execute(&pool).await.unwrap();
            }
        }
        pool
    }

    /// Mirror of the production wiring the controller will add to
    /// `build_router`: identical paths + permission gates.
    fn task2_router(state: AppState) -> axum::Router {
        use crate::auth::Permission;
        use axum::{
            middleware,
            routing::{delete, get, post},
        };
        let pool = state.pool.clone();
        let public = axum::Router::new()
            .route("/api/v1/auth/setup", post(crate::routes_auth::setup))
            .route("/api/v1/auth/login", post(crate::routes_auth::login));
        let protected = axum::Router::new()
            .route(
                "/api/v1/users",
                post(super::create_user).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::UsersManage),
                )),
            )
            .route(
                "/api/v1/users/:id",
                delete(super::delete_user).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::UsersManage),
                )),
            )
            .route(
                "/api/v1/admin/database/backup",
                get(super::backup_database).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::SettingsManage),
                )),
            )
            .route(
                "/api/v1/admin/database/restore",
                post(super::restore_database).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::SettingsManage),
                )),
            )
            .route_layer(middleware::from_fn_with_state(
                pool,
                crate::auth::auth_middleware,
            ));
        public.merge(protected).with_state(state)
    }

    struct AdminFixture {
        server: TestServer,
        pool: sqlx::SqlitePool,
        token: String,
        viewer_token: String,
        dir: std::path::PathBuf,
        live_db: std::path::PathBuf,
    }

    impl Drop for AdminFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn admin_fixture() -> AdminFixture {
        use serde_json::json;
        let dir = std::env::temp_dir().join(format!("pm-admin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = file_backed_pool(&dir).await;
        let root = dir.join("srvroot");
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState::for_tests(pool.clone(), root);
        let server = TestServer::new(task2_router(state)).unwrap();
        let res = server
            .post("/api/v1/auth/setup")
            .json(&json!({"username": "owner", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200, "setup: {}", res.text());
        let token = res.json::<serde_json::Value>()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let res = server
            .post("/api/v1/users")
            .authorization_bearer(&token)
            .json(&json!({"username": "viewer1", "password": "supersecret1", "role": "viewer"}))
            .await;
        assert_eq!(res.status_code(), 201, "create viewer: {}", res.text());
        let res = server
            .post("/api/v1/auth/login")
            .json(&json!({"username": "viewer1", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200, "viewer login: {}", res.text());
        let viewer_token = res.json::<serde_json::Value>()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let live_db = dir.join("pumpkin-manager.db");
        AdminFixture {
            server,
            pool,
            token,
            viewer_token,
            dir,
            live_db,
        }
    }

    fn restore_form(name: &str, bytes: Vec<u8>) -> axum_test::multipart::MultipartForm {
        use axum_test::multipart::{MultipartForm, Part};
        MultipartForm::new().add_part(
            "file",
            Part::bytes(bytes)
                .file_name(name.to_string())
                .mime_type("application/octet-stream"),
        )
    }

    async fn user_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn owner_row_id(pool: &sqlx::SqlitePool) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'owner'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Craft a standalone SQLite file for restore-upload tests.
    async fn craft_db(dir: &std::path::Path, name: &str, user_version: i64) -> Vec<u8> {
        let path = dir.join(name);
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE users (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE servers (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(&format!("PRAGMA user_version = {user_version}"))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        std::fs::read(&path).unwrap()
    }

    #[test]
    fn pending_restore_path_appends_suffix() {
        assert_eq!(
            super::pending_restore_path(std::path::Path::new("/data/pumpkin-manager.db")),
            std::path::PathBuf::from("/data/pumpkin-manager.db.pending-restore")
        );
    }

    #[tokio::test]
    async fn create_duplicate_409s() {
        use serde_json::json;
        let fx = admin_fixture().await;
        let body = json!({"username": "alice", "password": "supersecret1", "role": "admin"});
        let res = fx
            .server
            .post("/api/v1/users")
            .authorization_bearer(&fx.token)
            .json(&body)
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        let created: serde_json::Value = res.json();
        assert_eq!(created["username"], json!("alice"));
        assert_eq!(created["role"], json!("admin"));
        let res = fx
            .server
            .post("/api/v1/users")
            .authorization_bearer(&fx.token)
            .json(&body)
            .await;
        assert_eq!(res.status_code(), 409, "body: {}", res.text());
        assert!(res.text().contains("USER_EXISTS"));
        // Weak password and unknown role are 422.
        let res = fx
            .server
            .post("/api/v1/users")
            .authorization_bearer(&fx.token)
            .json(&json!({"username": "bob", "password": "short", "role": "admin"}))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
        let res = fx
            .server
            .post("/api/v1/users")
            .authorization_bearer(&fx.token)
            .json(&json!({"username": "bob", "password": "supersecret1", "role": "superadmin"}))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
    }

    #[tokio::test]
    async fn delete_last_owner_409s() {
        use serde_json::json;
        let fx = admin_fixture().await;
        let oid = owner_row_id(&fx.pool).await;
        // Sole active owner: refuse.
        let res = fx
            .server
            .delete(&format!("/api/v1/users/{oid}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(res.status_code(), 409, "body: {}", res.text());
        assert!(res.text().contains("LAST_OWNER"));
        // Second owner exists: self-delete of the first is allowed (204) and
        // revokes its sessions (the old token stops working).
        let res = fx
            .server
            .post("/api/v1/users")
            .authorization_bearer(&fx.token)
            .json(&json!({"username": "owner2", "password": "supersecret1", "role": "owner"}))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        let res = fx
            .server
            .delete(&format!("/api/v1/users/{oid}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(res.status_code(), 204, "body: {}", res.text());
        let res = fx
            .server
            .post("/api/v1/users")
            .authorization_bearer(&fx.token)
            .json(&json!({"username": "ghost", "password": "supersecret1", "role": "viewer"}))
            .await;
        assert_eq!(res.status_code(), 401, "body: {}", res.text());
    }

    #[tokio::test]
    async fn viewer_cannot_create_users_403() {
        use serde_json::json;
        let fx = admin_fixture().await;
        let res = fx
            .server
            .post("/api/v1/users")
            .authorization_bearer(&fx.viewer_token)
            .json(&json!({"username": "mallory", "password": "supersecret1", "role": "viewer"}))
            .await;
        assert_eq!(res.status_code(), 403, "body: {}", res.text());
        assert!(res.text().contains("PERMISSION_DENIED"));
        let res = fx
            .server
            .get("/api/v1/admin/database/backup")
            .authorization_bearer(&fx.viewer_token)
            .await;
        assert_eq!(res.status_code(), 403, "body: {}", res.text());
    }

    #[tokio::test]
    async fn restore_rejects_non_sqlite() {
        let fx = admin_fixture().await;
        let before = user_count(&fx.pool).await;
        let res = fx
            .server
            .post("/api/v1/admin/database/restore")
            .authorization_bearer(&fx.token)
            .multipart(restore_form(
                "evil.txt",
                b"definitely not a database".to_vec(),
            ))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
        assert_eq!(user_count(&fx.pool).await, before, "live DB touched");
        assert!(
            !super::pending_restore_path(&fx.live_db).exists(),
            "pending file staged"
        );
    }

    #[tokio::test]
    async fn restore_rejects_unknown_schema() {
        let fx = admin_fixture().await;
        let bytes = craft_db(&fx.dir, "newer.db", 99999).await;
        let before = user_count(&fx.pool).await;
        let res = fx
            .server
            .post("/api/v1/admin/database/restore")
            .authorization_bearer(&fx.token)
            .multipart(restore_form("newer.db", bytes))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
        assert_eq!(user_count(&fx.pool).await, before, "live DB touched");
        assert!(
            !super::pending_restore_path(&fx.live_db).exists(),
            "pending file staged"
        );
    }

    #[tokio::test]
    async fn restore_stages_valid_file() {
        let fx = admin_fixture().await;
        let bytes = craft_db(&fx.dir, "good.db", 0).await;
        assert!(bytes.starts_with(b"SQLite format 3\0"));
        let before = user_count(&fx.pool).await;
        let res = fx
            .server
            .post("/api/v1/admin/database/restore")
            .authorization_bearer(&fx.token)
            .multipart(restore_form("good.db", bytes.clone()))
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body: serde_json::Value = res.json();
        assert_eq!(
            body,
            serde_json::json!({"staged": true, "restart_required": true})
        );
        let pending = super::pending_restore_path(&fx.live_db);
        assert_eq!(std::fs::read(&pending).unwrap(), bytes);
        let aside: Vec<_> = std::fs::read_dir(&fx.dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().contains(".pre-restore-"))
            .collect();
        assert_eq!(aside.len(), 1, "pre-restore copy missing: {aside:?}");
        assert_eq!(user_count(&fx.pool).await, before, "live DB touched");
    }

    #[tokio::test]
    async fn backup_downloads_snapshot() {
        let fx = admin_fixture().await;
        let res = fx
            .server
            .get("/api/v1/admin/database/backup")
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let disposition = res
            .headers()
            .get("content-disposition")
            .expect("content-disposition header")
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            disposition.starts_with("attachment;"),
            "not an attachment: {disposition}"
        );
        assert!(
            disposition.contains("pumpkin-manager-") && disposition.ends_with(".db\""),
            "unexpected filename: {disposition}"
        );
        assert!(res.as_bytes().as_ref().starts_with(b"SQLite format 3\0"));
    }
}
