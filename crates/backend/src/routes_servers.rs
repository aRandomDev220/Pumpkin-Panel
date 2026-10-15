//! Server inventory + lifecycle endpoints (Phase 2).
//!
//! Contracts (frozen with `apps/web/src/lib/servers.ts`):
//! - `GET /servers` → `{servers: [...], total}` (paginated, capped)
//! - `POST /servers {name, slug?, game_port, bind_address?, image_reference,
//!   image_digest?, memory_limit_bytes?, cpu_limit?}` → `201` server +
//!   optional `warnings[]` (e.g. unpinned digest)
//! - `GET /servers/:id` → server object (flat)
//! - `DELETE /servers/:id?retain_data=bool` → `204`
//! - `POST /servers/:id/start|stop|restart` → `202 {task_id}`
//! - `GET /servers/:id/status` → desired/observed + live container view
//! - `GET /servers/:id/logs?tail=` → `{logs: [...]}` (Docker only)
//! - `GET /servers/:id/capabilities` → static capability profile from recon
//!
//! Long-running ops are tasks (202 + task id). Per-server op locks make
//! concurrent lifecycle calls fail fast with `SERVER_BUSY` instead of
//! racing Docker.

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::{
    audit,
    auth::AuthUser,
    runtime::{self, RuntimeAdapter, RuntimeError, ServerSpecInput, ValidatedServerSpec},
    task_runner, AppState,
};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ServerDto {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub runtime_type: String,
    pub runtime_version: String,
    pub image_reference: String,
    pub image_digest: String,
    pub container_id: String,
    pub data_path: String,
    pub game_port: i64,
    pub bind_address: String,
    pub desired_state: String,
    pub observed_state: String,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateServerRequest {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    pub game_port: u16,
    #[serde(default)]
    pub bind_address: Option<String>,
    pub image_reference: String,
    #[serde(default)]
    pub image_digest: Option<String>,
    #[serde(default)]
    pub memory_limit_bytes: Option<u64>,
    #[serde(default)]
    pub cpu_limit: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct Pagination {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    #[serde(default = "default_retain")]
    pub retain_data: bool,
}

fn default_retain() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_tail")]
    pub tail: usize,
}

fn default_tail() -> usize {
    200
}

#[derive(Debug, Serialize)]
pub struct TaskAccepted {
    pub task_id: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

fn slugify(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

async fn fetch_server(pool: &SqlitePool, id: &str) -> Result<Option<ServerDto>, sqlx::Error> {
    sqlx::query_as::<_, ServerDto>(
        "SELECT id, name, slug, description, runtime_type, runtime_version,
                image_reference, image_digest, container_id, data_path,
                game_port, bind_address, desired_state, observed_state,
                created_by, created_at, updated_at
         FROM servers WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Ports taken by live servers (game port and derived bedrock port).
async fn taken_ports(pool: &SqlitePool) -> Result<HashSet<i64>, sqlx::Error> {
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT game_port FROM servers WHERE deleted_at IS NULL")
            .fetch_all(pool)
            .await?;
    let mut set = HashSet::new();
    for (p,) in rows {
        set.insert(p);
        set.insert(p + 1); // bedrock convention: game_port + 1
    }
    Ok(set)
}

/// Try to claim the per-server op lock. `false` = conflicting op in flight.
async fn try_lock(locks: &Arc<Mutex<HashSet<String>>>, server_id: &str) -> bool {
    let mut guard = locks.lock().await;
    if guard.contains(server_id) {
        return false;
    }
    guard.insert(server_id.to_string());
    true
}

async fn unlock(locks: &Arc<Mutex<HashSet<String>>>, server_id: &str) {
    locks.lock().await.remove(server_id);
}

async fn conflicting_task(pool: &SqlitePool, server_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tasks
         WHERE server_id = ? AND state IN ('queued', 'running')",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
        > 0
}

fn runtime_error(e: RuntimeError) -> Response {
    // Daemon-down must read as degraded runtime, never as "stopped".
    if is_daemon_down(&e) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "RUNTIME_UNAVAILABLE",
            "Docker daemon unavailable; runtime state is degraded, not stopped",
        );
    }
    let code = e.code();
    let (status, msg) = match &e {
        RuntimeError::NotFound(_) => (StatusCode::NOT_FOUND, e.to_string()),
        RuntimeError::PortUnavailable(_) | RuntimeError::NameConflict(_) => {
            (StatusCode::CONFLICT, e.to_string())
        }
        _ => (StatusCode::BAD_GATEWAY, e.to_string()),
    };
    // Never leak raw daemon internals beyond the display string, which the
    // adapter keeps free of socket paths and secrets.
    error(status, code, &msg)
}

/// Heuristic for "Docker is unreachable" inside bollard's error strings.
fn is_daemon_down(e: &RuntimeError) -> bool {
    match e {
        RuntimeError::Docker(inner) => {
            let msg = inner.to_string();
            msg.contains("Socket")
                || msg.contains("socket")
                || msg.contains("connection refused")
                || msg.contains("No such file")
                || msg.contains("os error")
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/servers — paginated inventory.
pub async fn list_servers(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(pager): Query<Pagination>,
) -> Response {
    let limit = pager.limit.clamp(1, 100);
    let offset = pager.offset.max(0);
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers WHERE deleted_at IS NULL")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let servers = sqlx::query_as::<_, ServerDto>(
        "SELECT id, name, slug, description, runtime_type, runtime_version,
                image_reference, image_digest, container_id, data_path,
                game_port, bind_address, desired_state, observed_state,
                created_by, created_at, updated_at
         FROM servers WHERE deleted_at IS NULL
         ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    Json(serde_json::json!({ "servers": servers, "total": total })).into_response()
}

/// POST /api/v1/servers — validate, persist, materialise container.
pub async fn create_server(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateServerRequest>,
) -> Response {
    let request_id = audit::request_id();
    let name = body.name.trim().to_owned();
    if name.is_empty() || name.len() > 64 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_CONFIGURATION",
            "name must be 1-64 characters",
        );
    }
    let slug = body
        .slug
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| slugify(&name));
    if !runtime::is_valid_slug(&slug) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_CONFIGURATION",
            "slug must match ^[a-z0-9-]{3,48}$",
        );
    }
    let bind_address = body.bind_address.unwrap_or_else(|| "0.0.0.0".to_string());

    // Pre-check ports (DB UNIQUE is the backstop, this gives a clean 409).
    match taken_ports(&state.pool).await {
        Ok(taken) => {
            if taken.contains(&(body.game_port as i64))
                || taken.contains(&(body.game_port as i64 + 1))
            {
                return error(
                    StatusCode::CONFLICT,
                    "PORT_UNAVAILABLE",
                    "game port (or adjacent bedrock port) already allocated",
                );
            }
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to check port allocation",
            )
        }
    }

    let server_id = uuid::Uuid::new_v4().to_string();
    let data_path = state
        .server_root
        .join(&server_id)
        .to_string_lossy()
        .into_owned();
    let input = ServerSpecInput {
        server_id: server_id.parse().unwrap_or_else(|_| uuid::Uuid::new_v4()),
        name: name.clone(),
        slug: slug.clone(),
        image_reference: body.image_reference.trim().to_owned(),
        image_digest: body.image_digest.unwrap_or_default().trim().to_owned(),
        game_port: body.game_port,
        bind_address: bind_address.clone(),
        data_path: data_path.clone(),
        memory_limit_bytes: body.memory_limit_bytes.unwrap_or(2 * 1024 * 1024 * 1024),
        cpu_limit: body.cpu_limit.unwrap_or(2.0),
    };
    // Align the spec's server id with the allocated row id.
    let input = ServerSpecInput {
        server_id: server_id.parse().unwrap_or(input.server_id),
        ..input
    };
    let spec: ValidatedServerSpec = match runtime::validate_spec(input, &state.server_root) {
        Ok(s) => s,
        Err(e) => {
            let code = e.code();
            let msg = e.to_string();
            let status = if code == "PORT_UNAVAILABLE" {
                StatusCode::CONFLICT
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            return error(status, code, &msg);
        }
    };

    // Persist row + settings first (DB constraints are the dedup backstop).
    let insert = sqlx::query(
        "INSERT INTO servers
            (id, name, slug, runtime_type, image_reference, image_digest,
             data_path, game_port, bind_address, desired_state, observed_state, created_by)
         VALUES (?, ?, ?, 'pumpkin', ?, ?, ?, ?, ?, 'stopped', 'created', ?)",
    )
    .bind(&server_id)
    .bind(&name)
    .bind(&slug)
    .bind(&spec.image_reference)
    .bind(&spec.image_digest)
    .bind(&data_path)
    .bind(body.game_port as i64)
    .bind(&bind_address)
    .bind(&user.id)
    .execute(&state.pool)
    .await;
    if let Err(e) = insert {
        let msg = e.to_string();
        if msg.contains("UNIQUE constraint failed: servers.game_port") {
            return error(StatusCode::CONFLICT, "PORT_UNAVAILABLE", "game port taken");
        }
        if msg.contains("UNIQUE constraint failed") {
            return error(
                StatusCode::CONFLICT,
                "SERVER_CONFLICT",
                "name or slug already exists",
            );
        }
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to persist server",
        );
    }
    let _ = sqlx::query("INSERT INTO server_settings (server_id) VALUES (?)")
        .bind(&server_id)
        .execute(&state.pool)
        .await;

    // Materialise the container when a runtime is attached.
    let mut warnings: Vec<String> = Vec::new();
    if spec.image_digest.is_empty() {
        warnings.push(
            "No digest pinned: mutable tags can drift between pulls. \
             Pin @sha256:... for reproducible deploys."
                .to_string(),
        );
    }
    if let Some(adapter) = &state.adapter {
        match adapter.create(&spec).await {
            Ok(handle) => {
                let _ = sqlx::query(
                    "UPDATE servers SET container_id = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
                )
                .bind(&handle.container_id)
                .bind(&server_id)
                .execute(&state.pool)
                .await;
            }
            Err(e) => {
                // No half-created rows: roll back the inventory record.
                let _ = sqlx::query("DELETE FROM servers WHERE id = ?")
                    .bind(&server_id)
                    .execute(&state.pool)
                    .await;
                audit::record(
                    &state.pool,
                    Some(&user.id),
                    "server.create",
                    "server",
                    &server_id,
                    "failed",
                    &request_id,
                    serde_json::json!({ "name": name }),
                )
                .await;
                return runtime_error(e);
            }
        }
    }

    audit::record(
        &state.pool,
        Some(&user.id),
        "server.create",
        "server",
        &server_id,
        "ok",
        &request_id,
        serde_json::json!({ "name": name, "game_port": body.game_port }),
    )
    .await;
    match fetch_server(&state.pool, &server_id).await {
        Ok(Some(server)) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "server": server, "warnings": warnings })),
        )
            .into_response(),
        _ => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "server created but unreadable",
        ),
    }
}

/// GET /api/v1/servers/:id
pub async fn get_server(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    match fetch_server(&state.pool, &id).await {
        Ok(Some(server)) => Json(server).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server"),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to load server",
        ),
    }
}

/// DELETE /api/v1/servers/:id?retain_data= — remove container, soft-delete
/// row, optionally delete data. Synchronous (fast) in Phase 2; a task-based
/// delete lands with the update flow in Phase 7.
pub async fn delete_server(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Response {
    let request_id = audit::request_id();
    let Some(server) = fetch_server(&state.pool, &id).await.unwrap_or(None) else {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    };
    if !try_lock(&state.op_locks, &id).await {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another operation is in flight for this server",
        );
    }
    let result = delete_server_inner(&state, &server, query.retain_data).await;
    unlock(&state.op_locks, &id).await;
    match result {
        Ok(()) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "server.delete",
                "server",
                &id,
                "ok",
                &request_id,
                serde_json::json!({ "retain_data": query.retain_data }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(resp) => resp,
    }
}

// `Response` is axum's standard error vehicle; boxing it buys nothing.
#[allow(clippy::result_large_err)]
async fn delete_server_inner(
    state: &AppState,
    server: &ServerDto,
    retain_data: bool,
) -> Result<(), Response> {
    if !server.container_id.is_empty() {
        let Some(adapter) = &state.adapter else {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "RUNTIME_UNAVAILABLE",
                "no runtime attached; cannot remove container",
            ));
        };
        if let Err(e) = adapter.remove(&server.container_id).await {
            // Removing an already-gone container is success.
            if !matches!(e, RuntimeError::NotFound(_)) {
                return Err(runtime_error(e));
            }
        }
    }
    if !retain_data {
        // Best-effort recursive delete of this server's own data root only.
        // `data_path` was constructed under the server root at create time.
        // A never-materialised dir (no runtime) is already "deleted".
        let candidate = std::path::Path::new(&server.data_path);
        if candidate.exists() {
            let root = &state.server_root;
            let joined_root = root.canonicalize().unwrap_or_else(|_| root.clone());
            let canonical =
                std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_path_buf());
            if canonical.starts_with(&joined_root) && canonical != joined_root {
                if let Err(e) = std::fs::remove_dir_all(&canonical) {
                    tracing::warn!(server_id = %server.id, error = %e, "data delete failed");
                    return Err(error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "DELETE_FAILED",
                        "container removed but data delete failed; data retained on disk",
                    ));
                }
            }
        }
    }
    let _ = sqlx::query(
        "UPDATE servers SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(&server.id)
    .execute(&state.pool)
    .await;
    Ok(())
}

/// POST /api/v1/servers/:id/start|stop|restart — set desired state, enqueue
/// a task, return 202. Idempotent: start-on-running reports `already`
/// without spawning work.
pub async fn lifecycle(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, op)): Path<(String, String)>,
) -> Response {
    let request_id = audit::request_id();
    if !matches!(op.as_str(), "start" | "stop" | "restart") {
        return error(StatusCode::NOT_FOUND, "NOT_FOUND", "unknown lifecycle op");
    }
    let Some(server) = fetch_server(&state.pool, &id).await.unwrap_or(None) else {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    };
    if server.container_id.is_empty() {
        return error(
            StatusCode::CONFLICT,
            "MISSING_CONTAINER",
            "server has no container; recreate it first",
        );
    }
    // Idempotency: desired state already matches → report, don't enqueue.
    let want = if op == "stop" { "stopped" } else { "running" };
    if server.desired_state == want && op != "restart" {
        return Json(serde_json::json!({
            "status": "already",
            "desired_state": want,
        }))
        .into_response();
    }
    if !try_lock(&state.op_locks, &id).await {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another operation is in flight for this server",
        );
    }
    let outcome = lifecycle_inner(&state, &user, &server, &op, want).await;
    unlock(&state.op_locks, &id).await;
    match outcome {
        Ok(task_id) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                &format!("server.{op}"),
                "server",
                &id,
                "accepted",
                &request_id,
                serde_json::json!({ "task_id": task_id }),
            )
            .await;
            (StatusCode::ACCEPTED, Json(TaskAccepted { task_id })).into_response()
        }
        Err(resp) => resp,
    }
}

#[allow(clippy::result_large_err)] // see delete_server_inner
async fn lifecycle_inner(
    state: &AppState,
    user: &AuthUser,
    server: &ServerDto,
    op: &str,
    want: &str,
) -> Result<String, Response> {
    let (Some(adapter), Some(runner)) = (state.adapter.as_ref(), state.runner.as_ref()) else {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "RUNTIME_UNAVAILABLE",
            "no runtime attached; lifecycle unavailable",
        ));
    };
    let _ = adapter; // dispatch happens in the worker via the same adapter
    if conflicting_task(&state.pool, &server.id).await {
        return Err(error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "a task is already queued/running for this server",
        ));
    }
    let _ = sqlx::query(
        "UPDATE servers SET desired_state = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(want)
    .bind(&server.id)
    .execute(&state.pool)
    .await;
    match task_runner::enqueue_and_submit(&state.pool, runner, op, Some(&server.id), Some(&user.id))
        .await
    {
        Ok(record) => Ok(record.id),
        Err(e) => Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "TASK_QUEUE_FULL",
            &format!("task queue unavailable: {e}"),
        )),
    }
}

/// GET /api/v1/servers/:id/status — desired state, live observed state,
/// and game readiness from a protocol ping (never container existence
/// alone). A stopped server reports `stopped`, never `unhealthy`.
pub async fn server_status(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    let Some(server) = fetch_server(&state.pool, &id).await.unwrap_or(None) else {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    };
    let mut observed = server.observed_state.clone();
    let mut container: serde_json::Value = serde_json::Value::Null;
    let mut docker_available = state.adapter.is_some();
    let mut exit_code: Option<i64> = None;
    if let Some(adapter) = &state.adapter {
        if server.container_id.is_empty() {
            observed = "missing".to_string();
        } else {
            match adapter.inspect(&server.container_id).await {
                Ok(snap) => {
                    observed = map_container_status(&snap.status);
                    exit_code = snap.exit_code;
                    container = serde_json::json!({
                        "container_id": snap.container_id,
                        "status": snap.status,
                        "exit_code": snap.exit_code,
                        "health": snap.health,
                    });
                    let _ = sqlx::query("UPDATE servers SET observed_state = ? WHERE id = ?")
                        .bind(&observed)
                        .bind(&server.id)
                        .execute(&state.pool)
                        .await;
                }
                Err(RuntimeError::NotFound(_)) => {
                    observed = "missing".to_string();
                }
                Err(e) if is_daemon_down(&e) => {
                    docker_available = false;
                }
                Err(e) => {
                    tracing::warn!(server_id = %server.id, error = %e, "status inspect failed");
                }
            }
        }
    }
    // Game readiness: ping the Java port when the container claims to run.
    // `bind_address` may be a LAN IP unreachable from here, so ping
    // localhost — the published port answers there for 0.0.0.0 bindings.
    let mut ping_json = serde_json::Value::Null;
    if observed == "running" {
        match crate::readiness::ping_java(
            "127.0.0.1",
            server.game_port as u16,
            std::time::Duration::from_secs(3),
        )
        .await
        {
            Ok(ping) => {
                ping_json = serde_json::json!({
                    "players_online": ping.players_online,
                    "players_max": ping.players_max,
                    "version": ping.version_name,
                    "motd": ping.motd_text,
                    "latency_ms": ping.latency_ms,
                });
            }
            Err(e) => {
                tracing::debug!(server_id = %server.id, error = ?e, "readiness ping failed");
            }
        }
    }
    let ping_ok = !ping_json.is_null();
    let (readiness_state, reason) =
        readiness_for(&server.desired_state, &observed, exit_code, ping_ok);
    Json(serde_json::json!({
        "id": server.id,
        "desired_state": server.desired_state,
        "observed_state": observed,
        "readiness": {
            "state": readiness_state,
            "reason": reason,
            "ping": ping_json,
        },
        "docker_available": docker_available,
        "container": container,
    }))
    .into_response()
}

/// Derive game readiness from desired/observed container state.
/// `online` is only reachable with a live ping (set by the caller).
fn readiness_for(
    desired: &str,
    observed: &str,
    exit_code: Option<i64>,
    ping_ok: bool,
) -> (&'static str, String) {
    if observed == "running" && ping_ok {
        return ("online", "game port answered status ping".to_string());
    }
    match (desired, observed) {
        (_, "running") => (
            "starting",
            "container running, awaiting game ping".to_string(),
        ),
        ("stopped", _) => ("stopped", "server is stopped".to_string()),
        (_, "created") => ("starting", "container created, not started yet".to_string()),
        (_, "exited") => (
            "unhealthy",
            format!(
                "process exited while running was requested{}",
                exit_code
                    .map(|c| format!(" (exit {c})"))
                    .unwrap_or_default()
            ),
        ),
        (_, "missing") => (
            "unknown",
            "no container materialised for this server".to_string(),
        ),
        _ => ("unknown", format!("observed state {observed}")),
    }
}

fn map_container_status(docker_status: &str) -> String {
    match docker_status {
        "running" => "running",
        "created" => "created",
        "paused" => "paused",
        "restarting" => "starting",
        "removing" => "stopping",
        "exited" | "dead" => "exited",
        _ => "unknown",
    }
    .to_string()
}

/// GET /api/v1/servers/:id/tasks — task history for one server:
/// failures and activity for the detail view. Newest first, capped.
pub async fn server_tasks(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Query(pager): Query<Pagination>,
) -> Response {
    if fetch_server(&state.pool, &id)
        .await
        .unwrap_or(None)
        .is_none()
    {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    let limit = pager.limit.clamp(1, 100);
    let offset = pager.offset.max(0);
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE server_id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let rows = sqlx::query(
        "SELECT id, type, state, server_id, requested_by, progress_percent,
                message, error_code, error_summary
         FROM tasks WHERE server_id = ?
         ORDER BY created_at DESC LIMIT ? OFFSET ?",
    )
    .bind(&id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await;
    match rows {
        Ok(rows) => {
            use sqlx::Row as _;
            let tasks: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.get::<String, _>("id"),
                        "kind": r.get::<String, _>("type"),
                        "type": r.get::<String, _>("type"),
                        "state": r.get::<String, _>("state"),
                        "server_id": r.get::<Option<String>, _>("server_id"),
                        "requested_by": r.get::<Option<String>, _>("requested_by"),
                        "progress_percent": r.get::<i64, _>("progress_percent"),
                        "message": r.get::<String, _>("message"),
                        "error": r.get::<Option<String>, _>("error_summary"),
                        "error_code": r.get::<Option<String>, _>("error_code"),
                        "error_summary": r.get::<Option<String>, _>("error_summary"),
                    })
                })
                .collect();
            Json(serde_json::json!({ "tasks": tasks, "total": total })).into_response()
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to list tasks",
        ),
    }
}

/// GET /api/v1/servers/:id/logs?tail= — Docker log tail (read-only).
pub async fn server_logs(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Response {
    let Some(server) = fetch_server(&state.pool, &id).await.unwrap_or(None) else {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    };
    if server.container_id.is_empty() {
        return error(
            StatusCode::NOT_FOUND,
            "LOGS_UNAVAILABLE",
            "server has no container yet",
        );
    }
    let Some(adapter) = &state.adapter else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "RUNTIME_UNAVAILABLE",
            "no runtime attached",
        );
    };
    match adapter
        .logs(&server.container_id, query.tail.min(2000))
        .await
    {
        Ok(lines) => Json(serde_json::json!({ "logs": lines })).into_response(),
        Err(e) => runtime_error(e),
    }
}

/// GET /api/v1/servers/:id/capabilities — static profile from
/// `docs/compatibility/pumpkin-runtime.md`. Values here are the only
/// ones the UI may use to gate features.
pub async fn server_capabilities(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    if fetch_server(&state.pool, &id)
        .await
        .unwrap_or(None)
        .is_none()
    {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    Json(serde_json::json!({
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
    }))
    .into_response()
}
