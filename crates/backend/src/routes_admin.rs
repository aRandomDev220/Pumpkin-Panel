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
// Reconcile
// ---------------------------------------------------------------------------

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
