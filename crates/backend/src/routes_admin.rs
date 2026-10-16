//! Admin surface (Phase 2): task inspection, user management, audit
//! trail, and on-demand reconcile. All routes sit behind auth +
//! fine-grained permissions at router wiring time.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

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
}
