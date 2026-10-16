//! Phase 7 — Pumpkin image update + rollback worker.
//!
//! Updates run as `update`/`rollback` task types through the existing
//! [`task_runner`] dispatch (arms added there call
//! [`execute_update`]/[`execute_rollback`] with the runner's adapter):
//!
//! * [`preview_update`] — read-only preflight: allowlist violation is an
//!   error, everything else (unpinned tag, unknown Pumpkin profile, missing
//!   verified backup) is a warning. Never pulls, never mutates.
//! * [`enqueue_update`] — gates (`409 UPDATE_NO_BACKUP` unless a completed
//!   verified backup exists or the caller passes the explicit flag,
//!   `422 INVALID_IMAGE` outside the allowlist, `409 SERVER_BUSY` when any
//!   task is queued/running for the server) then enqueues an `update` task
//!   whose target image travels in the task `message` as JSON (the `tasks`
//!   table has no payload column; the worker parses it before claiming the
//!   task). Returns the task id.
//! * [`enqueue_rollback`] — `409 NO_ROLLBACK_TARGET` when
//!   `previous_image_reference` is empty. Seeds the task message with the
//!   data-format one-way warning so it is present even before the worker
//!   runs; every terminal rollback message carries it too.
//! * Worker steps (inside `update` execution): worker-side op-lock → disk
//!   preflight (free ≥ 2× data dir size, measured on the configured backup
//!   filesystem) → mandatory protected auto-backup via the canonical
//!   [`routes_backup::run_backup_capture`] (same code path as
//!   `enqueue_backup`: `full`/`live`/protected row, reason `pre-update`,
//!   same checksum/verify semantics; the backup dir is resolved from
//!   `Settings::backup_dir`, never from `data_path`) → pull in the worker →
//!   stop → remove → persist `previous_image_*` FIRST, then swap
//!   `image_reference/digest` → create with the NEW image → start → readiness
//!   (container `running` + 20s Java-ping window). Success KEEPS the previous
//!   reference (only the next update overwrites it). On ANY failure after the
//!   remove, the worker recreates + starts with the previous image and the
//!   task message states whether that rollback succeeded. Missing
//!   `0007_update` columns fail loudly (`UPDATE_PRECONDITION`) instead of
//!   silently dropping the rollback target.
//!
//! Handlers ([`preview_image_update`], [`update_server`], [`rollback_server`])
//! are standalone `pub async fn` taking `State<AppState>`; route wiring and
//! permission layers are the controller's job.

// The preview/enqueue/handler surface below is wired by the controller in
// `main.rs`, so it reads as dead code to the binary build until then.
// Drop this once the routes are wired (same pattern as scheduler.rs).
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    audit,
    auth::AuthUser,
    config::Settings,
    config_profile, readiness,
    routes_backup::{self, BackupError},
    runtime::{
        DockerAdapter, RuntimeAdapter, RuntimeError, ValidatedServerSpec, IMAGE_ALLOWLIST_PREFIX,
    },
    task_runner, AppState,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Task type strings, executed via [`task_runner::dispatch_task`].
const TASK_TYPE_UPDATE: &str = "update";
const TASK_TYPE_ROLLBACK: &str = "rollback";

/// Post-start game-readiness window (Java status ping, 2s attempts).
const READINESS_WINDOW_SECS: u64 = 20;
/// How long the worker waits for a fresh container to report `running`.
const START_GRACE_SECS: u64 = 10;

/// Fallback resources when no `server_settings` row exists (mirror the table
/// defaults: 2 GiB, 2.0 vCPU).
const DEFAULT_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_CPU_LIMIT: f64 = 2.0;

/// Rollback always restores the previous *container image*, but world data
/// migrated by a newer version may not come back. Attached to every rollback
/// message (queued and terminal).
const DATA_FORMAT_WARNING: &str = "Warning: world data format changes are one-way — rolling back restores the previous server image, but world data already migrated by the newer version may be unreadable or lost.";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Failures from the update/rollback surface. Carries its own HTTP mapping so
/// handlers and the future controller wiring agree on codes.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// No live server row for the id.
    #[error("server not found")]
    ServerNotFound,
    /// Image outside the allowlist (or malformed digest).
    #[error("invalid image: {0}")]
    InvalidImage(String),
    /// No completed verified backup and no explicit opt-out.
    #[error("no completed verified backup exists for this server")]
    NoBackup,
    /// `previous_image_reference` is empty.
    #[error("no previous image recorded for rollback")]
    NoRollbackTarget,
    /// Another task is queued/running, or the op lock is held.
    #[error("another operation is in flight for this server")]
    ServerBusy,
    /// Required schema/table state is missing (e.g. `0007_update` columns).
    /// Fails loudly so an update can never silently drop its rollback target.
    #[error("update precondition failed: {0}")]
    Precondition(String),
    /// Task bookkeeping failure.
    #[error("task error: {0}")]
    Task(String),
    /// SQLite failure.
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<task_runner::TaskError> for UpdateError {
    fn from(e: task_runner::TaskError) -> Self {
        match e {
            task_runner::TaskError::Database(db) => Self::Db(db),
            other => Self::Task(other.to_string()),
        }
    }
}

impl UpdateError {
    /// Stable API error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ServerNotFound => "SERVER_NOT_FOUND",
            Self::InvalidImage(_) => "INVALID_IMAGE",
            Self::NoBackup => "UPDATE_NO_BACKUP",
            Self::NoRollbackTarget => "NO_ROLLBACK_TARGET",
            Self::ServerBusy => "SERVER_BUSY",
            Self::Precondition(_) => "UPDATE_PRECONDITION",
            Self::Task(_) | Self::Db(_) => "INTERNAL",
        }
    }

    /// HTTP status for the failure.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::ServerNotFound => StatusCode::NOT_FOUND,
            Self::InvalidImage(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NoBackup | Self::NoRollbackTarget | Self::ServerBusy => StatusCode::CONFLICT,
            Self::Precondition(_) | Self::Task(_) | Self::Db(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

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

fn update_error(e: UpdateError) -> Response {
    error(e.status(), e.code(), &e.to_string())
}

// ---------------------------------------------------------------------------
// Rows + small queries
// ---------------------------------------------------------------------------

/// Server columns the update worker needs. A missing `0007_update` migration
/// is a loud [`UpdateError::Precondition`], never a silent fallback.
struct ServerRow {
    id: String,
    name: String,
    slug: String,
    image_reference: String,
    image_digest: String,
    previous_image_reference: String,
    previous_image_digest: String,
    container_id: String,
    data_path: String,
    game_port: i64,
    bind_address: String,
    desired_state: String,
}

async fn fetch_server(pool: &SqlitePool, id: &str) -> Result<Option<ServerRow>, UpdateError> {
    use sqlx::Row;
    const FULL: &str = "SELECT id, name, slug, image_reference, image_digest, \
        previous_image_reference, previous_image_digest, container_id, data_path, \
        game_port, bind_address, desired_state \
        FROM servers WHERE id = ? AND deleted_at IS NULL";
    let row = sqlx::query(FULL)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            if e.to_string().contains("no such column") {
                UpdateError::Precondition(
                    "servers table is missing previous_image_* columns (0007_update not applied)"
                        .to_string(),
                )
            } else {
                UpdateError::Db(e)
            }
        })?;
    row.map(|r| {
        Ok::<_, sqlx::Error>(ServerRow {
            id: r.try_get("id")?,
            name: r.try_get("name")?,
            slug: r.try_get("slug")?,
            image_reference: r.try_get("image_reference")?,
            image_digest: r.try_get("image_digest")?,
            previous_image_reference: r.try_get("previous_image_reference")?,
            previous_image_digest: r.try_get("previous_image_digest")?,
            container_id: r.try_get("container_id")?,
            data_path: r.try_get("data_path")?,
            game_port: r.try_get("game_port")?,
            bind_address: r.try_get("bind_address")?,
            desired_state: r.try_get("desired_state")?,
        })
    })
    .transpose()
    .map_err(UpdateError::Db)
}

/// `(memory_limit_bytes, cpu_limit)` from `server_settings`, else defaults.
async fn server_resources(pool: &SqlitePool, server_id: &str) -> (u64, f64) {
    let row: Option<(i64, f64)> = sqlx::query_as(
        "SELECT memory_limit_bytes, cpu_limit FROM server_settings WHERE server_id = ?",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    match row {
        Some((m, c)) if m > 0 && c > 0.0 => (m as u64, c),
        _ => (DEFAULT_MEMORY_BYTES, DEFAULT_CPU_LIMIT),
    }
}

/// A backup counts as verified when it completed with a recorded checksum.
/// A missing `backups` table (minimal test schemas) means "no backup".
async fn has_verified_backup(pool: &SqlitePool, server_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM backups
          WHERE server_id = ? AND state = 'completed' AND checksum != ''",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
        > 0
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

/// Persist previous + current image refs in one UPDATE. A missing
/// `0007_update` migration fails loudly instead of silently dropping the
/// rollback target.
async fn swap_images(
    pool: &SqlitePool,
    id: &str,
    prev_ref: &str,
    prev_digest: &str,
    cur_ref: &str,
    cur_digest: &str,
) -> Result<(), UpdateError> {
    sqlx::query(
        "UPDATE servers SET previous_image_reference = ?, previous_image_digest = ?,
                image_reference = ?, image_digest = ?,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(prev_ref)
    .bind(prev_digest)
    .bind(cur_ref)
    .bind(cur_digest)
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("no such column") {
            UpdateError::Precondition(
                "servers table is missing previous_image_* columns (0007_update not applied)"
                    .to_string(),
            )
        } else {
            UpdateError::Db(e)
        }
    })?;
    Ok(())
}

async fn set_container(pool: &SqlitePool, id: &str, container_id: &str) {
    let _ = sqlx::query(
        "UPDATE servers SET container_id = ?,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(container_id)
    .bind(id)
    .execute(pool)
    .await;
}

async fn set_observed(pool: &SqlitePool, id: &str, observed: &str) {
    let _ = sqlx::query(
        "UPDATE servers SET observed_state = ?,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(observed)
    .bind(id)
    .execute(pool)
    .await;
}

// ---------------------------------------------------------------------------
// Worker-side op locks + misc primitives
// ---------------------------------------------------------------------------

/// Process-wide per-server locks for the update worker. `dispatch_task` only
/// hands workers `pool + adapter` (no `AppState`), so the worker cannot reuse
/// `AppState::op_locks`; this mirrors that pattern for the synchronous
/// worker section. Enqueue-time safety comes from [`conflicting_task`].
static WORKER_LOCKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn worker_locks() -> &'static Mutex<HashSet<String>> {
    WORKER_LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

async fn op_claim(server_id: &str) -> bool {
    let mut guard = worker_locks().lock().await;
    if guard.contains(server_id) {
        return false;
    }
    guard.insert(server_id.to_string());
    true
}

async fn op_release(server_id: &str) {
    worker_locks().lock().await.remove(server_id);
}

/// Handler-level lock on the shared [`AppState::op_locks`] (same pattern as
/// `routes_servers` / `routes_backup`).
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

/// Heuristic for "Docker is unreachable" (mirrors `routes_backup`).
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

/// Sum of regular-file sizes under `path` (blocking, symlink-free walk).
fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(md) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if md.is_dir() {
                stack.push(entry.path());
            } else if md.is_file() {
                total = total.saturating_add(md.len());
            }
        }
    }
    total
}

/// Free bytes on the filesystem holding `path` (`None` → proceed
/// optimistically).
fn free_bytes_for(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut best: Option<(usize, u64)> = None;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if target.starts_with(mount) {
            let depth = mount.components().count();
            if best.map(|(d, _)| depth > d).unwrap_or(true) {
                best = Some((depth, disk.available_space()));
            }
        }
    }
    best.map(|(_, free)| free)
}

/// Backup root, resolved EXACTLY the way `enqueue_backup` callers do:
/// `Settings::load().backup_dir` (env `PUMPKIN_MANAGER_BACKUP_DIR` feeds
/// `Settings`; default `/data/backups`). Never derived from `data_path`, so
/// a customized backup dir, backup listing/GC, and the disk preflight all
/// agree on one location.
fn resolve_backup_dir() -> Result<PathBuf, String> {
    Settings::load()
        .map(|s| s.backup_dir)
        .map_err(|e| format!("cannot load backup dir settings: {e}"))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn check_allowlist(image_reference: &str) -> Result<(), UpdateError> {
    if image_reference.starts_with(IMAGE_ALLOWLIST_PREFIX) {
        Ok(())
    } else {
        Err(UpdateError::InvalidImage(format!(
            "image '{image_reference}' is not on the allowlist (must start with {IMAGE_ALLOWLIST_PREFIX})"
        )))
    }
}

/// Normalised digest (`""` when unpinned). Malformed digests are invalid.
fn normalize_digest(digest: Option<&str>) -> Result<String, UpdateError> {
    let d = digest.unwrap_or("").trim().to_string();
    if !d.is_empty() && !(d.starts_with("sha256:") && d.len() > 7) {
        return Err(UpdateError::InvalidImage(
            "image digest must be empty or a sha256:<hex> digest".to_string(),
        ));
    }
    Ok(d)
}

/// Tag portion of a reference (`pumpkin:master` → `master`), for the Pumpkin
/// profile check.
fn image_tag(reference: &str) -> &str {
    let base = reference.split('@').next().unwrap_or(reference);
    let last = base.rsplit('/').next().unwrap_or(base);
    last.split(':').nth(1).unwrap_or(last)
}

// ---------------------------------------------------------------------------
// Preview + enqueue
// ---------------------------------------------------------------------------

/// Read-only update preflight (never pulls, never mutates).
#[derive(Debug, Clone, Serialize)]
pub struct UpdatePreview {
    /// Non-blocking warnings: unpinned tag, unknown profile, no backup.
    pub warnings: Vec<String>,
    /// Requested digest, when pinned.
    pub resolved_digest: Option<String>,
    /// The server's current image reference.
    pub current_reference: String,
}

/// Preview an image update: allowlist violations (and malformed digests) are
/// errors; everything else is a warning.
pub async fn preview_update(
    pool: &SqlitePool,
    server_id: &str,
    image_reference: &str,
    image_digest: Option<&str>,
) -> Result<UpdatePreview, UpdateError> {
    let server = fetch_server(pool, server_id)
        .await?
        .ok_or(UpdateError::ServerNotFound)?;
    check_allowlist(image_reference)?;
    let digest = normalize_digest(image_digest)?;

    let mut warnings = Vec::new();
    if digest.is_empty() {
        warnings.push(
            "target image tag is not pinned with a digest; the tag could shift between preview and pull"
                .to_string(),
        );
    }
    let tag = image_tag(image_reference);
    if !config_profile::profile_for_version(tag).known {
        warnings.push(format!(
            "no known Pumpkin profile for image tag '{tag}'; configuration defaults may not match this version"
        ));
    }
    if !has_verified_backup(pool, server_id).await {
        warnings.push(
            "no verified backup exists for this server; the update will need a fresh backup or the allow-without-backup flag"
                .to_string(),
        );
    }
    Ok(UpdatePreview {
        warnings,
        resolved_digest: if digest.is_empty() {
            None
        } else {
            Some(digest)
        },
        current_reference: server.image_reference,
    })
}

/// Target image travelling in the `update` task message (JSON — the `tasks`
/// table has no payload column).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateTarget {
    image_reference: String,
    #[serde(default)]
    image_digest: String,
}

/// Enqueue an image update. Returns the task id. The adapter is accepted for
/// wiring symmetry but unused here — the pull happens in the worker.
#[allow(clippy::too_many_arguments)]
pub async fn enqueue_update(
    pool: &SqlitePool,
    _adapter: Option<&DockerAdapter>,
    server_id: &str,
    requested_by: Option<&str>,
    image_reference: &str,
    image_digest: Option<&str>,
    allow_without_backup: bool,
) -> Result<String, UpdateError> {
    fetch_server(pool, server_id)
        .await?
        .ok_or(UpdateError::ServerNotFound)?;
    // Allowlist first: a typosquat image is rejected before any backup
    // demand and long before any pull could happen.
    check_allowlist(image_reference)?;
    let digest = normalize_digest(image_digest)?;
    // Busy before backup-presence so a racing op reports SERVER_BUSY, not a
    // misleading UPDATE_NO_BACKUP.
    if conflicting_task(pool, server_id).await {
        return Err(UpdateError::ServerBusy);
    }
    if !allow_without_backup && !has_verified_backup(pool, server_id).await {
        return Err(UpdateError::NoBackup);
    }

    let record =
        task_runner::enqueue(pool, TASK_TYPE_UPDATE, Some(server_id), requested_by).await?;
    let target = UpdateTarget {
        image_reference: image_reference.to_string(),
        image_digest: digest,
    };
    let _ = sqlx::query("UPDATE tasks SET message = ? WHERE id = ?")
        .bind(serde_json::to_string(&target).unwrap_or_default())
        .bind(&record.id)
        .execute(pool)
        .await;
    Ok(record.id)
}

/// Enqueue a rollback to `previous_image_reference`. The queued task message
/// already carries the data-format one-way warning.
pub async fn enqueue_rollback(
    pool: &SqlitePool,
    _adapter: Option<&DockerAdapter>,
    server_id: &str,
    requested_by: Option<&str>,
) -> Result<String, UpdateError> {
    let server = fetch_server(pool, server_id)
        .await?
        .ok_or(UpdateError::ServerNotFound)?;
    if server.previous_image_reference.is_empty() {
        return Err(UpdateError::NoRollbackTarget);
    }
    if conflicting_task(pool, server_id).await {
        return Err(UpdateError::ServerBusy);
    }

    let record =
        task_runner::enqueue(pool, TASK_TYPE_ROLLBACK, Some(server_id), requested_by).await?;
    let _ = sqlx::query("UPDATE tasks SET message = ? WHERE id = ?")
        .bind(format!(
            "rollback to {} queued. {DATA_FORMAT_WARNING}",
            server.previous_image_reference
        ))
        .bind(&record.id)
        .execute(pool)
        .await;
    Ok(record.id)
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// Terminal success marker.
async fn terminal_ok(pool: &SqlitePool, task_id: &str, message: &str) {
    let _ = task_runner::mark_succeeded(pool, task_id, message).await;
}

/// Terminal failure marker. `mark_failed` only writes `error_*`, so the full
/// story is stored in `message` first — rollback outcomes must be readable
/// from the task message itself.
async fn terminal_fail(pool: &SqlitePool, task_id: &str, code: &str, summary: &str) {
    let summary: String = summary.chars().take(500).collect();
    let _ = sqlx::query("UPDATE tasks SET message = ? WHERE id = ?")
        .bind(&summary)
        .bind(task_id)
        .execute(pool)
        .await;
    let _ = task_runner::mark_failed(pool, task_id, code, &summary).await;
}

fn audit_update(
    pool: &SqlitePool,
    actor: Option<&str>,
    action: &str,
    target_id: &str,
    outcome: &str,
    metadata: serde_json::Value,
) -> impl std::future::Future<Output = ()> + Send {
    let pool = pool.clone();
    let actor = actor.map(str::to_string);
    let action = action.to_string();
    let target_id = target_id.to_string();
    let outcome = outcome.to_string();
    async move {
        audit::record(
            &pool,
            actor.as_deref(),
            &action,
            "server",
            &target_id,
            &outcome,
            &audit::request_id(),
            metadata,
        )
        .await;
    }
}

fn build_spec(
    server: &ServerRow,
    memory_limit_bytes: u64,
    cpu_limit: f64,
    image_reference: &str,
    image_digest: &str,
) -> Result<ValidatedServerSpec, String> {
    let server_id = Uuid::parse_str(&server.id).map_err(|e| format!("invalid server id: {e}"))?;
    let game_port: u16 = server
        .game_port
        .try_into()
        .map_err(|_| format!("invalid game port {}", server.game_port))?;
    let bedrock_port = game_port
        .checked_add(1)
        .ok_or_else(|| format!("game port {game_port} leaves no room for the bedrock port"))?;
    Ok(ValidatedServerSpec {
        server_id,
        name: server.name.clone(),
        slug: server.slug.clone(),
        image_reference: image_reference.to_string(),
        image_digest: image_digest.to_string(),
        game_port,
        bedrock_port,
        bind_address: server.bind_address.clone(),
        data_path: server.data_path.clone(),
        memory_limit_bytes,
        cpu_limit,
    })
}

/// Wait for the container to report `running`, then ping the Java port until
/// the game answers or the 20s window expires.
async fn wait_ready<A: RuntimeAdapter>(
    adapter: &A,
    container_id: &str,
    game_port: u16,
) -> Result<(), String> {
    let grace = Instant::now() + Duration::from_secs(START_GRACE_SECS);
    loop {
        match adapter.inspect(container_id).await {
            Ok(snap) if snap.status == "running" => break,
            Ok(snap) => {
                if Instant::now() >= grace {
                    return Err(format!(
                        "container did not reach running (status '{}')",
                        snap.status
                    ));
                }
            }
            Err(e) => return Err(format!("container inspect failed: {e}")),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(READINESS_WINDOW_SECS);
    loop {
        match readiness::ping_java("127.0.0.1", game_port, Duration::from_secs(2)).await {
            Ok(_) => return Ok(()),
            Err(_) => {
                if Instant::now() >= deadline {
                    return Err("game server did not answer the status ping within 20s".to_string());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Map a canonical backup-capture failure onto an update task code.
fn backup_task_code(e: &BackupError) -> &'static str {
    match e {
        BackupError::Cancelled => "TASK_CANCELLED",
        BackupError::NoSpace => "UPDATE_NO_SPACE",
        _ => "BACKUP_FAILED",
    }
}

/// Recreate + start with the previous image after a post-remove failure.
/// Always terminal: the update failed; the message states whether the
/// automatic rollback succeeded.
#[allow(clippy::too_many_arguments)]
async fn auto_rollback<A: RuntimeAdapter>(
    pool: &SqlitePool,
    adapter: &A,
    task_id: &str,
    server_id: &str,
    prev_ref: &str,
    prev_digest: &str,
    new_ref: &str,
    new_digest: &str,
    failed_stage: &str,
    reason: &str,
    should_start: bool,
    game_port: u16,
    memory_limit_bytes: u64,
    cpu_limit: f64,
    server_name: &str,
    server_slug: &str,
    bind_address: &str,
    data_path: &str,
    actor: Option<&str>,
) {
    let proxy = ServerRow {
        id: server_id.to_string(),
        name: server_name.to_string(),
        slug: server_slug.to_string(),
        image_reference: String::new(),
        image_digest: String::new(),
        previous_image_reference: String::new(),
        previous_image_digest: String::new(),
        container_id: String::new(),
        data_path: data_path.to_string(),
        game_port: i64::from(game_port),
        bind_address: bind_address.to_string(),
        desired_state: String::new(),
    };
    let spec = match build_spec(&proxy, memory_limit_bytes, cpu_limit, prev_ref, prev_digest) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!(
                "update to {new_ref} failed at {failed_stage}: {reason}; automatic rollback to {prev_ref} FAILED (cannot rebuild spec: {e}). Restore the pre-update backup. {DATA_FORMAT_WARNING}"
            );
            terminal_fail(pool, task_id, "UPDATE_FAILED", &msg).await;
            audit_update(
                pool,
                actor,
                "update.rollback",
                server_id,
                "failed",
                serde_json::json!({ "stage": failed_stage, "error": e }),
            )
            .await;
            return;
        }
    };
    let handle = match adapter.create(&spec).await {
        Ok(h) => h,
        Err(e) => {
            let msg = format!(
                "update to {new_ref} failed at {failed_stage}: {reason}; automatic rollback to {prev_ref} FAILED (recreate: {e}). The server may have no container — restore the pre-update backup. {DATA_FORMAT_WARNING}"
            );
            terminal_fail(pool, task_id, "UPDATE_FAILED", &msg).await;
            audit_update(
                pool,
                actor,
                "update.rollback",
                server_id,
                "failed",
                serde_json::json!({ "stage": failed_stage, "error": e.to_string() }),
            )
            .await;
            return;
        }
    };
    set_container(pool, server_id, &handle.container_id).await;
    // The container runs the previous image again: record previous=new so a
    // further rollback/manual retry still knows both sides.
    if let Err(e) = swap_images(pool, server_id, new_ref, new_digest, prev_ref, prev_digest).await {
        terminal_fail(
            pool,
            task_id,
            e.code(),
            &format!(
                "update to {new_ref} failed at {failed_stage}: {reason}; rolled back to {prev_ref} (container {}) but image refs could not be persisted: {e}. {DATA_FORMAT_WARNING}",
                handle.container_id
            ),
        )
        .await;
        return;
    }
    if should_start {
        if let Err(e) = adapter.start(&handle.container_id).await {
            let msg = format!(
                "update to {new_ref} failed at {failed_stage}: {reason}; rolled back to {prev_ref} (container {}) but start failed: {e}. Start it manually or restore the pre-update backup. {DATA_FORMAT_WARNING}",
                handle.container_id
            );
            terminal_fail(pool, task_id, "UPDATE_FAILED", &msg).await;
            set_observed(pool, server_id, "created").await;
            audit_update(
                pool,
                actor,
                "update.rollback",
                server_id,
                "failed",
                serde_json::json!({ "stage": "rollback-start", "error": e.to_string() }),
            )
            .await;
            return;
        }
        if let Err(e) = wait_ready(adapter, &handle.container_id, game_port).await {
            let msg = format!(
                "update to {new_ref} failed at {failed_stage}: {reason}; rolled back to {prev_ref} and it started, but readiness failed: {e}. Check the console or restore the pre-update backup. {DATA_FORMAT_WARNING}"
            );
            terminal_fail(pool, task_id, "UPDATE_FAILED", &msg).await;
            set_observed(pool, server_id, "running").await;
            audit_update(
                pool,
                actor,
                "update.rollback",
                server_id,
                "failed",
                serde_json::json!({ "stage": "rollback-readiness", "error": e }),
            )
            .await;
            return;
        }
        set_observed(pool, server_id, "running").await;
    } else {
        set_observed(pool, server_id, "created").await;
    }
    let msg = format!(
        "update to {new_ref} failed at {failed_stage}: {reason}; automatically rolled back to {prev_ref} and it is ready. {DATA_FORMAT_WARNING}"
    );
    terminal_fail(pool, task_id, "UPDATE_FAILED", &msg).await;
    audit_update(
        pool,
        actor,
        "update.rollback",
        server_id,
        "ok",
        serde_json::json!({ "stage": failed_stage, "restored": prev_ref }),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn execute_update_inner<A: RuntimeAdapter>(
    pool: &SqlitePool,
    adapter: &A,
    task_id: &str,
    server_id: &str,
    target: &UpdateTarget,
    actor: Option<&str>,
) {
    let Some(server) = (match fetch_server(pool, server_id).await {
        Ok(s) => s,
        Err(e) => {
            terminal_fail(pool, task_id, e.code(), &format!("update failed: {e}")).await;
            return;
        }
    }) else {
        terminal_fail(
            pool,
            task_id,
            "SERVER_NOT_FOUND",
            &format!("update failed: server {server_id} not found"),
        )
        .await;
        return;
    };
    if server.data_path.is_empty() {
        terminal_fail(
            pool,
            task_id,
            "UPDATE_FAILED",
            "update failed: server has no data path",
        )
        .await;
        return;
    }
    if check_allowlist(&target.image_reference).is_err() {
        terminal_fail(
            pool,
            task_id,
            "INVALID_IMAGE",
            &format!(
                "update failed: image '{}' is not on the allowlist",
                target.image_reference
            ),
        )
        .await;
        return;
    }
    let game_port: u16 = match server.game_port.try_into() {
        Ok(p) => p,
        Err(_) => {
            terminal_fail(
                pool,
                task_id,
                "UPDATE_FAILED",
                &format!("update failed: invalid game port {}", server.game_port),
            )
            .await;
            return;
        }
    };
    let (memory_limit_bytes, cpu_limit) = server_resources(pool, server_id).await;
    audit_update(
        pool,
        actor,
        "update.start",
        server_id,
        "started",
        serde_json::json!({ "task_id": task_id, "target": target.image_reference }),
    )
    .await;

    // Preflight: free disk on the CONFIGURED backup filesystem must hold
    // ~2x the data dir.
    let data_path = PathBuf::from(&server.data_path);
    let data_size = tokio::task::spawn_blocking(move || dir_size_bytes(&data_path))
        .await
        .unwrap_or(0);
    let backup_root = match resolve_backup_dir() {
        Ok(dir) => dir,
        Err(e) => {
            terminal_fail(
                pool,
                task_id,
                "BACKUP_FAILED",
                &format!("update failed: {e}"),
            )
            .await;
            return;
        }
    };
    if data_size > 0 {
        if let Some(free) = free_bytes_for(&backup_root) {
            if free < data_size.saturating_mul(2) {
                terminal_fail(
                    pool,
                    task_id,
                    "UPDATE_NO_SPACE",
                    &format!(
                        "update failed: insufficient disk space (need ~{} bytes free)",
                        data_size.saturating_mul(2)
                    ),
                )
                .await;
                audit_update(
                    pool,
                    actor,
                    "update.preflight",
                    server_id,
                    "failed",
                    serde_json::json!({ "error": "insufficient disk space" }),
                )
                .await;
                return;
            }
        }
    }

    // Mandatory protected auto-backup (reason: pre-update) through the
    // canonical backup capture — the same code path `enqueue_backup` runs
    // (tar, staging, checksum, verify, row lifecycle, audits).
    // `enqueue_backup` itself cannot run here: its `conflicting_task` gate
    // would self-conflict with this running update task.
    let backup_id = Uuid::new_v4().to_string();
    if let Err(e) = sqlx::query(
        "INSERT INTO backups
            (id, server_id, type, state, consistency_method, protected, created_by)
          VALUES (?, ?, 'full', 'in_progress', 'live', 1, ?)",
    )
    .bind(&backup_id)
    .bind(server_id)
    .bind(actor)
    .execute(pool)
    .await
    {
        terminal_fail(
            pool,
            task_id,
            "BACKUP_FAILED",
            &format!("update failed: cannot record pre-update backup: {e}"),
        )
        .await;
        return;
    }
    audit_update(
        pool,
        actor,
        "update.pre_backup",
        server_id,
        "started",
        serde_json::json!({ "task_id": task_id, "backup_id": backup_id, "reason": "pre-update" }),
    )
    .await;
    let backup_server = routes_backup::ServerRow {
        id: server.id.clone(),
        data_path: server.data_path.clone(),
        container_id: server.container_id.clone(),
        desired_state: server.desired_state.clone(),
    };
    match routes_backup::run_backup_capture(
        pool,
        None,
        &backup_root,
        &backup_id,
        &backup_server,
        "live",
        task_id,
        actor,
    )
    .await
    {
        Ok((outcome, _resume)) => {
            audit_update(
                pool,
                actor,
                "update.pre_backup",
                server_id,
                "ok",
                serde_json::json!({
                    "task_id": task_id,
                    "backup_id": backup_id,
                    "reason": "pre-update",
                    "size_bytes": outcome.size_bytes,
                    "checksum": outcome.checksum,
                }),
            )
            .await;
        }
        Err(BackupError::Cancelled) => {
            let _ = task_runner::mark_cancelled(pool, task_id).await;
            audit_update(
                pool,
                actor,
                "update.pre_backup",
                server_id,
                "cancelled",
                serde_json::json!({ "task_id": task_id, "backup_id": backup_id }),
            )
            .await;
            return;
        }
        Err(e) => {
            let _ =
                sqlx::query("UPDATE backups SET state = 'failed', error_summary = ? WHERE id = ?")
                    .bind(e.to_string().chars().take(500).collect::<String>())
                    .bind(&backup_id)
                    .execute(pool)
                    .await;
            terminal_fail(
                pool,
                task_id,
                backup_task_code(&e),
                &format!("update failed: pre-update backup failed, nothing was changed: {e}"),
            )
            .await;
            audit_update(
                pool,
                actor,
                "update.pre_backup",
                server_id,
                "failed",
                serde_json::json!({ "task_id": task_id, "backup_id": backup_id, "error": e.to_string() }),
            )
            .await;
            return;
        }
    }

    // Pull the target image in the worker.
    let pull_ref = if target.image_digest.is_empty() {
        target.image_reference.clone()
    } else {
        format!("{}@{}", target.image_reference, target.image_digest)
    };
    if let Err(e) = adapter.pull_image(&pull_ref).await {
        if is_daemon_down(&e) {
            terminal_fail(
                pool,
                task_id,
                "RUNTIME_UNAVAILABLE",
                "update failed: docker unavailable during image pull",
            )
            .await;
        } else {
            terminal_fail(
                pool,
                task_id,
                "IMAGE_PULL_FAILED",
                &format!("update failed: could not pull image {pull_ref}: {e}"),
            )
            .await;
        }
        audit_update(
            pool,
            actor,
            "update.pull",
            server_id,
            "failed",
            serde_json::json!({ "target": pull_ref, "error": e.to_string() }),
        )
        .await;
        return;
    }
    audit_update(
        pool,
        actor,
        "update.pull",
        server_id,
        "ok",
        serde_json::json!({ "target": pull_ref }),
    )
    .await;

    // Stop the current container (best-effort when already gone).
    let mut was_running = server.desired_state == "running";
    let mut container_gone = server.container_id.is_empty();
    if !container_gone {
        match adapter.inspect(&server.container_id).await {
            Ok(snap) => was_running = snap.status == "running",
            Err(RuntimeError::NotFound(_)) => container_gone = true,
            Err(e) if is_daemon_down(&e) => {
                terminal_fail(
                    pool,
                    task_id,
                    "RUNTIME_UNAVAILABLE",
                    "update failed: docker unavailable during pre-update inspect",
                )
                .await;
                return;
            }
            Err(e) => {
                terminal_fail(
                    pool,
                    task_id,
                    "UPDATE_FAILED",
                    &format!("update failed: cannot inspect current container: {e}"),
                )
                .await;
                return;
            }
        }
    }
    if !container_gone && was_running {
        if let Err(e) = adapter
            .stop(&server.container_id, task_runner::TASK_STOP_TIMEOUT_SECS)
            .await
        {
            match e {
                RuntimeError::NotFound(_) => container_gone = true,
                _ if is_daemon_down(&e) => {
                    terminal_fail(
                        pool,
                        task_id,
                        "RUNTIME_UNAVAILABLE",
                        "update failed: docker unavailable during stop",
                    )
                    .await;
                    return;
                }
                _ => {
                    terminal_fail(
                        pool,
                        task_id,
                        "UPDATE_FAILED",
                        &format!("update failed: cannot stop current container: {e}"),
                    )
                    .await;
                    return;
                }
            }
        }
    }

    // Remove. Past this point every failure rolls back to the previous image.
    if !container_gone {
        if let Err(e) = adapter.remove(&server.container_id).await {
            if !matches!(e, RuntimeError::NotFound(_)) {
                if was_running {
                    let _ = adapter.start(&server.container_id).await;
                }
                terminal_fail(pool, task_id, "UPDATE_FAILED", &format!("update failed: cannot remove current container (old container left in place): {e}")).await;
                return;
            }
        }
    }

    // Persist previous FIRST, then swap to the target.
    let old_ref = server.image_reference.clone();
    let old_digest = server.image_digest.clone();
    if let Err(e) = swap_images(
        pool,
        server_id,
        &old_ref,
        &old_digest,
        &target.image_reference,
        &target.image_digest,
    )
    .await
    {
        terminal_fail(
            pool,
            task_id,
            e.code(),
            &format!("update failed: cannot persist image refs: {e}"),
        )
        .await;
        return;
    }
    set_container(pool, server_id, "").await;
    audit_update(
        pool,
        actor,
        "update.recreate",
        server_id,
        "started",
        serde_json::json!({ "previous": old_ref, "target": target.image_reference }),
    )
    .await;

    let should_start = was_running || server.desired_state == "running";
    let new_spec = match build_spec(
        &server,
        memory_limit_bytes,
        cpu_limit,
        &target.image_reference,
        &target.image_digest,
    ) {
        Ok(s) => s,
        Err(e) => {
            auto_rollback(
                pool,
                adapter,
                task_id,
                server_id,
                &old_ref,
                &old_digest,
                &target.image_reference,
                &target.image_digest,
                "spec",
                &e,
                should_start,
                game_port,
                memory_limit_bytes,
                cpu_limit,
                &server.name,
                &server.slug,
                &server.bind_address,
                &server.data_path,
                actor,
            )
            .await;
            return;
        }
    };
    let handle = match adapter.create(&new_spec).await {
        Ok(h) => h,
        Err(e) => {
            auto_rollback(
                pool,
                adapter,
                task_id,
                server_id,
                &old_ref,
                &old_digest,
                &target.image_reference,
                &target.image_digest,
                "create",
                &e.to_string(),
                should_start,
                game_port,
                memory_limit_bytes,
                cpu_limit,
                &server.name,
                &server.slug,
                &server.bind_address,
                &server.data_path,
                actor,
            )
            .await;
            return;
        }
    };
    set_container(pool, server_id, &handle.container_id).await;
    if should_start {
        if let Err(e) = adapter.start(&handle.container_id).await {
            auto_rollback(
                pool,
                adapter,
                task_id,
                server_id,
                &old_ref,
                &old_digest,
                &target.image_reference,
                &target.image_digest,
                "start",
                &e.to_string(),
                should_start,
                game_port,
                memory_limit_bytes,
                cpu_limit,
                &server.name,
                &server.slug,
                &server.bind_address,
                &server.data_path,
                actor,
            )
            .await;
            return;
        }
        if let Err(e) = wait_ready(adapter, &handle.container_id, game_port).await {
            auto_rollback(
                pool,
                adapter,
                task_id,
                server_id,
                &old_ref,
                &old_digest,
                &target.image_reference,
                &target.image_digest,
                "readiness",
                &e,
                should_start,
                game_port,
                memory_limit_bytes,
                cpu_limit,
                &server.name,
                &server.slug,
                &server.bind_address,
                &server.data_path,
                actor,
            )
            .await;
            return;
        }
        set_observed(pool, server_id, "running").await;
    } else {
        set_observed(pool, server_id, "created").await;
    }
    // Success: KEEP previous_image_* for a future rollback.
    terminal_ok(
        pool,
        task_id,
        &format!(
            "update to {} complete (previous {} kept for rollback)",
            target.image_reference, old_ref
        ),
    )
    .await;
    audit_update(pool, actor, "update.complete", server_id, "ok",
        serde_json::json!({ "task_id": task_id, "image": target.image_reference, "previous": old_ref })).await;
}

/// Dispatch entry for `update` tasks (called from [`task_runner`]).
pub async fn execute_update<A: RuntimeAdapter>(
    pool: &SqlitePool,
    adapter: &A,
    task_id: &str,
) -> Result<(), task_runner::TaskError> {
    let Some(task) = task_runner::get(pool, task_id).await? else {
        return Err(task_runner::TaskError::NotFound(task_id.to_string()));
    };
    let Some(server_id) = task.server_id.clone() else {
        task_runner::mark_failed(
            pool,
            task_id,
            "INVALID_CONFIGURATION",
            "update task has no server_id",
        )
        .await?;
        return Ok(());
    };
    let target: UpdateTarget = match serde_json::from_str(&task.message) {
        Ok(t) => t,
        Err(_) => {
            task_runner::mark_failed(
                pool,
                task_id,
                "UPDATE_FAILED",
                "update task has no target image",
            )
            .await?;
            return Ok(());
        }
    };
    if !op_claim(&server_id).await {
        terminal_fail(
            pool,
            task_id,
            "SERVER_BUSY",
            "update failed: another operation is in flight for this server",
        )
        .await;
        return Ok(());
    }
    task_runner::mark_running(pool, task_id, "update in progress").await?;
    execute_update_inner(
        pool,
        adapter,
        task_id,
        &server_id,
        &target,
        task.requested_by.as_deref(),
    )
    .await;
    op_release(&server_id).await;
    Ok(())
}

async fn execute_rollback_inner<A: RuntimeAdapter>(
    pool: &SqlitePool,
    adapter: &A,
    task_id: &str,
    server_id: &str,
    actor: Option<&str>,
) {
    let Some(server) = (match fetch_server(pool, server_id).await {
        Ok(s) => s,
        Err(e) => {
            terminal_fail(
                pool,
                task_id,
                e.code(),
                &format!("rollback failed: {e}. {DATA_FORMAT_WARNING}"),
            )
            .await;
            return;
        }
    }) else {
        terminal_fail(
            pool,
            task_id,
            "SERVER_NOT_FOUND",
            &format!("rollback failed: server {server_id} not found. {DATA_FORMAT_WARNING}"),
        )
        .await;
        return;
    };
    if server.previous_image_reference.is_empty() {
        terminal_fail(pool, task_id, "NO_ROLLBACK_TARGET", &format!("rollback failed: no previous image recorded for this server. {DATA_FORMAT_WARNING}")).await;
        return;
    }
    let prev_ref = server.previous_image_reference.clone();
    let prev_digest = server.previous_image_digest.clone();
    let cur_ref = server.image_reference.clone();
    let cur_digest = server.image_digest.clone();
    let game_port: u16 = match server.game_port.try_into() {
        Ok(p) => p,
        Err(_) => {
            terminal_fail(
                pool,
                task_id,
                "ROLLBACK_FAILED",
                &format!(
                    "rollback failed: invalid game port {}. {DATA_FORMAT_WARNING}",
                    server.game_port
                ),
            )
            .await;
            return;
        }
    };
    let (memory_limit_bytes, cpu_limit) = server_resources(pool, server_id).await;
    audit_update(
        pool,
        actor,
        "rollback.start",
        server_id,
        "started",
        serde_json::json!({ "task_id": task_id, "target": prev_ref }),
    )
    .await;

    let mut was_running = server.desired_state == "running";
    let mut container_gone = server.container_id.is_empty();
    if !container_gone {
        match adapter.inspect(&server.container_id).await {
            Ok(snap) => was_running = snap.status == "running",
            Err(RuntimeError::NotFound(_)) => container_gone = true,
            Err(e) if is_daemon_down(&e) => {
                terminal_fail(
                    pool,
                    task_id,
                    "RUNTIME_UNAVAILABLE",
                    &format!("rollback failed: docker unavailable. {DATA_FORMAT_WARNING}"),
                )
                .await;
                return;
            }
            Err(e) => {
                terminal_fail(pool, task_id, "ROLLBACK_FAILED", &format!("rollback failed: cannot inspect current container: {e}. {DATA_FORMAT_WARNING}")).await;
                return;
            }
        }
    }
    if !container_gone && was_running {
        if let Err(e) = adapter
            .stop(&server.container_id, task_runner::TASK_STOP_TIMEOUT_SECS)
            .await
        {
            match e {
                RuntimeError::NotFound(_) => container_gone = true,
                _ => {
                    terminal_fail(pool, task_id, "ROLLBACK_FAILED", &format!("rollback failed: cannot stop current container: {e}. {DATA_FORMAT_WARNING}")).await;
                    return;
                }
            }
        }
    }
    if !container_gone {
        if let Err(e) = adapter.remove(&server.container_id).await {
            if !matches!(e, RuntimeError::NotFound(_)) {
                terminal_fail(pool, task_id, "ROLLBACK_FAILED", &format!("rollback failed: cannot remove current container (left in place): {e}. {DATA_FORMAT_WARNING}")).await;
                return;
            }
        }
    }

    // Past the remove: recreate with the previous image, then swap the refs
    // (current ↔ previous) so the undone image stays reachable.
    let spec = match build_spec(
        &server,
        memory_limit_bytes,
        cpu_limit,
        &prev_ref,
        &prev_digest,
    ) {
        Ok(s) => s,
        Err(e) => {
            terminal_fail(pool, task_id, "ROLLBACK_FAILED", &format!("rollback failed: cannot rebuild spec for {prev_ref}: {e}. {DATA_FORMAT_WARNING}")).await;
            return;
        }
    };
    let handle = match adapter.create(&spec).await {
        Ok(h) => h,
        Err(e) => {
            // Best effort: put the pre-rollback container back.
            let mut extra = String::new();
            if let Ok(retry) = build_spec(
                &server,
                memory_limit_bytes,
                cpu_limit,
                &cur_ref,
                &cur_digest,
            ) {
                match adapter.create(&retry).await {
                    Ok(h) => {
                        set_container(pool, server_id, &h.container_id).await;
                        extra = format!(" The pre-rollback image {cur_ref} was recreated (container {}); start it manually.", h.container_id);
                    }
                    Err(e2) => {
                        extra = format!(" Recreating the pre-rollback image {cur_ref} also failed: {e2}; restore from a backup.");
                    }
                }
            }
            terminal_fail(pool, task_id, "ROLLBACK_FAILED", &format!("rollback failed: cannot recreate container with {prev_ref}: {e}.{extra} {DATA_FORMAT_WARNING}")).await;
            audit_update(
                pool,
                actor,
                "rollback.complete",
                server_id,
                "failed",
                serde_json::json!({ "error": e.to_string() }),
            )
            .await;
            return;
        }
    };
    set_container(pool, server_id, &handle.container_id).await;
    if let Err(e) = swap_images(
        pool,
        server_id,
        &cur_ref,
        &cur_digest,
        &prev_ref,
        &prev_digest,
    )
    .await
    {
        terminal_fail(pool, task_id, e.code(), &format!("rollback to {prev_ref} recreated (container {}) but image refs could not be persisted: {e}. {DATA_FORMAT_WARNING}", handle.container_id)).await;
        return;
    }
    let should_start = was_running || server.desired_state == "running";
    if should_start {
        if let Err(e) = adapter.start(&handle.container_id).await {
            terminal_fail(pool, task_id, "ROLLBACK_FAILED", &format!("rollback to {prev_ref} recreated but start failed: {e}. Start it manually. {DATA_FORMAT_WARNING}")).await;
            set_observed(pool, server_id, "created").await;
            return;
        }
        if let Err(e) = wait_ready(adapter, &handle.container_id, game_port).await {
            terminal_fail(pool, task_id, "ROLLBACK_FAILED", &format!("rollback to {prev_ref} started but readiness failed: {e}. Check the console. {DATA_FORMAT_WARNING}")).await;
            set_observed(pool, server_id, "running").await;
            return;
        }
        set_observed(pool, server_id, "running").await;
    } else {
        set_observed(pool, server_id, "created").await;
    }
    terminal_ok(
        pool,
        task_id,
        &format!("rollback to {prev_ref} complete. {DATA_FORMAT_WARNING}"),
    )
    .await;
    audit_update(
        pool,
        actor,
        "rollback.complete",
        server_id,
        "ok",
        serde_json::json!({ "task_id": task_id, "image": prev_ref }),
    )
    .await;
}

/// Dispatch entry for `rollback` tasks (called from [`task_runner`]).
pub async fn execute_rollback<A: RuntimeAdapter>(
    pool: &SqlitePool,
    adapter: &A,
    task_id: &str,
) -> Result<(), task_runner::TaskError> {
    let Some(task) = task_runner::get(pool, task_id).await? else {
        return Err(task_runner::TaskError::NotFound(task_id.to_string()));
    };
    let Some(server_id) = task.server_id.clone() else {
        terminal_fail(
            pool,
            task_id,
            "INVALID_CONFIGURATION",
            "rollback task has no server_id",
        )
        .await;
        return Ok(());
    };
    if !op_claim(&server_id).await {
        terminal_fail(pool, task_id, "SERVER_BUSY", &format!("rollback failed: another operation is in flight for this server. {DATA_FORMAT_WARNING}")).await;
        return Ok(());
    }
    task_runner::mark_running(pool, task_id, "rollback in progress").await?;
    execute_rollback_inner(
        pool,
        adapter,
        task_id,
        &server_id,
        task.requested_by.as_deref(),
    )
    .await;
    op_release(&server_id).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers (wired + permission-gated by the controller in `main.rs`)
// ---------------------------------------------------------------------------

/// POST /api/v1/servers/:id/update/preview body.
#[derive(Debug, Deserialize)]
pub struct PreviewUpdateRequest {
    /// Target image reference (must be on the allowlist).
    pub image_reference: String,
    /// Optional pinned digest (`sha256:…`).
    #[serde(default)]
    pub image_digest: Option<String>,
}

/// POST /api/v1/servers/:id/update body.
#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    /// Target image reference (must be on the allowlist).
    pub image_reference: String,
    /// Optional pinned digest (`sha256:…`).
    #[serde(default)]
    pub image_digest: Option<String>,
    /// Skip the `UPDATE_NO_BACKUP` gate (operator accepts the risk).
    #[serde(default)]
    pub allow_without_backup: bool,
}

/// POST /servers/:id/update/preview — read-only preflight.
pub async fn preview_image_update(
    State(state): State<AppState>,
    _user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<PreviewUpdateRequest>,
) -> Response {
    match preview_update(
        &state.pool,
        &id,
        body.image_reference.trim(),
        body.image_digest.as_deref(),
    )
    .await
    {
        Ok(p) => Json(serde_json::json!({
            "warnings": p.warnings,
            "resolved_digest": p.resolved_digest,
            "current_reference": p.current_reference,
        }))
        .into_response(),
        Err(e) => update_error(e),
    }
}

/// POST /servers/:id/update — enqueue an image update, `202 {task_id}`.
pub async fn update_server(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UpdateRequest>,
) -> Response {
    let request_id = audit::request_id();
    if !try_lock(&state.op_locks, &id).await {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another operation is in flight for this server",
        );
    }
    let outcome = enqueue_update(
        &state.pool,
        state.adapter.as_deref(),
        &id,
        Some(&user.id),
        body.image_reference.trim(),
        body.image_digest.as_deref(),
        body.allow_without_backup,
    )
    .await;
    unlock(&state.op_locks, &id).await;
    match outcome {
        Ok(task_id) => {
            let Some(runner) = &state.runner else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "RUNTIME_UNAVAILABLE",
                    "no task runner attached; update unavailable",
                );
            };
            if state.adapter.is_none() {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "RUNTIME_UNAVAILABLE",
                    "no runtime attached; update unavailable",
                );
            }
            if let Err(e) = runner.submit(&task_id).await {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "TASK_QUEUE_FULL",
                    &format!("task queue unavailable: {e}"),
                );
            }
            audit::record(
                &state.pool,
                Some(&user.id),
                "update.enqueue",
                "server",
                &id,
                "accepted",
                &request_id,
                serde_json::json!({ "task_id": task_id, "target": body.image_reference }),
            )
            .await;
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "task_id": task_id })),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "update.enqueue",
                "server",
                &id,
                "failed",
                &request_id,
                serde_json::json!({ "error_code": e.code() }),
            )
            .await;
            update_error(e)
        }
    }
}

/// POST /servers/:id/rollback — enqueue a rollback, `202 {task_id}`.
pub async fn rollback_server(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let request_id = audit::request_id();
    if !try_lock(&state.op_locks, &id).await {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another operation is in flight for this server",
        );
    }
    let outcome =
        enqueue_rollback(&state.pool, state.adapter.as_deref(), &id, Some(&user.id)).await;
    unlock(&state.op_locks, &id).await;
    match outcome {
        Ok(task_id) => {
            let Some(runner) = &state.runner else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "RUNTIME_UNAVAILABLE",
                    "no task runner attached; rollback unavailable",
                );
            };
            if state.adapter.is_none() {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "RUNTIME_UNAVAILABLE",
                    "no runtime attached; rollback unavailable",
                );
            }
            if let Err(e) = runner.submit(&task_id).await {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "TASK_QUEUE_FULL",
                    &format!("task queue unavailable: {e}"),
                );
            }
            audit::record(
                &state.pool,
                Some(&user.id),
                "rollback.enqueue",
                "server",
                &id,
                "accepted",
                &request_id,
                serde_json::json!({ "task_id": task_id }),
            )
            .await;
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "task_id": task_id })),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "rollback.enqueue",
                "server",
                &id,
                "failed",
                &request_id,
                serde_json::json!({ "error_code": e.code() }),
            )
            .await;
            update_error(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests (sqlite :memory:, single connection; no daemon)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        sqlx::query(
            "CREATE TABLE servers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                slug TEXT NOT NULL UNIQUE,
                image_reference TEXT NOT NULL DEFAULT '',
                image_digest TEXT NOT NULL DEFAULT '',
                previous_image_reference TEXT NOT NULL DEFAULT '',
                previous_image_digest TEXT NOT NULL DEFAULT '',
                container_id TEXT NOT NULL DEFAULT '',
                data_path TEXT NOT NULL DEFAULT '',
                game_port INTEGER NOT NULL UNIQUE,
                bind_address TEXT NOT NULL DEFAULT '0.0.0.0',
                desired_state TEXT NOT NULL DEFAULT 'stopped',
                observed_state TEXT NOT NULL DEFAULT 'created',
                deleted_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("servers table");
        sqlx::query(
            "CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                server_id TEXT REFERENCES servers(id) ON DELETE CASCADE,
                requested_by TEXT,
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
            )",
        )
        .execute(&pool)
        .await
        .expect("tasks table");
        sqlx::query(
            "CREATE TABLE backups (
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
                created_by TEXT,
                error_summary TEXT,
                protected INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .expect("backups table");
        sqlx::query(
            "CREATE TABLE server_settings (
                server_id TEXT PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
                memory_limit_bytes INTEGER NOT NULL DEFAULT 2147483648,
                cpu_limit REAL NOT NULL DEFAULT 2.0
            )",
        )
        .execute(&pool)
        .await
        .expect("settings table");
        sqlx::query(
            "CREATE TABLE audit_events (
                id TEXT PRIMARY KEY,
                actor_user_id TEXT,
                action TEXT NOT NULL,
                target_type TEXT NOT NULL DEFAULT '',
                target_id TEXT NOT NULL DEFAULT '',
                outcome TEXT NOT NULL DEFAULT 'ok',
                request_id TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                metadata TEXT NOT NULL DEFAULT '{}'
            )",
        )
        .execute(&pool)
        .await
        .expect("audit table");
        pool
    }

    async fn seed_server(pool: &SqlitePool, id: &str, port: i64, data_path: &str) {
        sqlx::query(
            "INSERT INTO servers (id, name, slug, image_reference, image_digest,
                    container_id, data_path, game_port)
              VALUES (?, ?, ?, 'ghcr.io/pumpkin-mc/pumpkin:master', '', '', ?, ?)",
        )
        .bind(id)
        .bind(format!("name-{id}"))
        .bind(format!("slug-{id}"))
        .bind(data_path)
        .bind(port)
        .execute(pool)
        .await
        .expect("seed server");
    }

    const TARGET: &str = "ghcr.io/pumpkin-mc/pumpkin:v99.0";

    #[tokio::test]
    async fn update_rejects_non_allowlist_before_pull() {
        let pool = test_pool().await;
        seed_server(&pool, "srv-1", 25565, "/tmp/pumpkin-test-srv-1").await;
        // No adapter at all (no daemon possible): a typosquat image must
        // still be rejected as INVALID_IMAGE, before any backup demand and
        // long before any pull could happen.
        let err = enqueue_update(
            &pool,
            None,
            "srv-1",
            None,
            "docker.io/evil/pumpkin:latest",
            None,
            true,
        )
        .await
        .expect_err("typosquat image must be rejected");
        assert_eq!(err.code(), "INVALID_IMAGE");
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // And preview rejects it too (error, not warning).
        let perr = preview_update(&pool, "srv-1", "docker.io/evil/pumpkin:latest", None)
            .await
            .expect_err("preview must reject non-allowlist");
        assert_eq!(perr.code(), "INVALID_IMAGE");
    }

    #[tokio::test]
    async fn update_requires_backup_or_flag() {
        let pool = test_pool().await;
        seed_server(&pool, "srv-1", 25565, "/tmp/pumpkin-test-srv-1").await;
        let err = enqueue_update(&pool, None, "srv-1", None, TARGET, None, false)
            .await
            .expect_err("update without a backup must be gated");
        assert_eq!(err.code(), "UPDATE_NO_BACKUP");
        assert_eq!(err.status(), StatusCode::CONFLICT);

        // Explicit opt-out is accepted and yields a runnable task id.
        let task_id = enqueue_update(&pool, None, "srv-1", None, TARGET, None, true)
            .await
            .expect("flagged update accepted");
        assert!(!task_id.is_empty());
        // Retire it so the next enqueue is not gated by our own task.
        task_runner::mark_succeeded(&pool, &task_id, "done")
            .await
            .expect("retire");

        // A completed verified backup also opens the gate.
        sqlx::query(
            "INSERT INTO backups (id, server_id, state, checksum) VALUES ('b1', 'srv-1', 'completed', 'abc')",
        )
        .execute(&pool)
        .await
        .expect("seed backup");
        enqueue_update(&pool, None, "srv-1", None, TARGET, None, false)
            .await
            .expect("update with verified backup accepted");
    }

    #[tokio::test]
    async fn rollback_without_target_409s() {
        let pool = test_pool().await;
        seed_server(&pool, "srv-1", 25565, "/tmp/pumpkin-test-srv-1").await;
        let err = enqueue_rollback(&pool, None, "srv-1", None)
            .await
            .expect_err("rollback without a target must fail");
        assert_eq!(err.code(), "NO_ROLLBACK_TARGET");
        assert_eq!(err.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn rollback_warns_data_format() {
        let pool = test_pool().await;
        seed_server(&pool, "srv-1", 25565, "/tmp/pumpkin-test-srv-1").await;
        sqlx::query(
            "UPDATE servers SET previous_image_reference = 'ghcr.io/pumpkin-mc/pumpkin:v1'
              WHERE id = 'srv-1'",
        )
        .execute(&pool)
        .await
        .expect("seed previous");
        let task_id = enqueue_rollback(&pool, None, "srv-1", None)
            .await
            .expect("rollback with a target accepted");
        let task = task_runner::get(&pool, &task_id)
            .await
            .expect("get")
            .expect("row");
        assert!(
            task.message.contains("one-way"),
            "queued rollback message must carry the one-way warning, got: {}",
            task.message
        );
    }

    #[tokio::test]
    async fn concurrent_update_and_backup_conflict() {
        let pool = test_pool().await;
        seed_server(&pool, "srv-1", 25565, "/tmp/pumpkin-test-srv-1").await;
        sqlx::query(
            "INSERT INTO tasks (id, type, server_id, requested_by, state)
              VALUES ('t-backup', 'backup', 'srv-1', 'u1', 'queued')",
        )
        .execute(&pool)
        .await
        .expect("seed backup task");
        let err = enqueue_update(&pool, None, "srv-1", None, TARGET, None, true)
            .await
            .expect_err("update racing a backup must be rejected");
        assert_eq!(err.code(), "SERVER_BUSY");
        let rerr = enqueue_rollback(&pool, None, "srv-1", None)
            .await
            .expect_err("rollback racing a backup must be rejected");
        // No previous image here, so the target gate fires first — seed one
        // and confirm the busy gate is what blocks rollback.
        assert_eq!(rerr.code(), "NO_ROLLBACK_TARGET");
        sqlx::query(
            "UPDATE servers SET previous_image_reference = 'ghcr.io/pumpkin-mc/pumpkin:v1'
              WHERE id = 'srv-1'",
        )
        .execute(&pool)
        .await
        .expect("seed previous");
        let rerr = enqueue_rollback(&pool, None, "srv-1", None)
            .await
            .expect_err("rollback racing a backup must be busy");
        assert_eq!(rerr.code(), "SERVER_BUSY");
    }

    // --- dispatch-level tests (no daemon; fake adapter) ---

    /// Every [`RuntimeAdapter`] method fails: with no daemon, nothing past
    /// the pull can succeed, which pins pull-before-mutate ordering.
    struct NoDaemon;

    #[allow(clippy::manual_async_fn)] // trait declares `-> impl Future`; async fn can't express it here
    impl RuntimeAdapter for NoDaemon {
        fn create(
            &self,
            _spec: &ValidatedServerSpec,
        ) -> impl std::future::Future<Output = Result<crate::runtime::RuntimeHandle, RuntimeError>> + Send
        {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
        fn start(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<(), RuntimeError>> + Send {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
        fn stop(
            &self,
            _container_id: &str,
            _timeout_secs: u32,
        ) -> impl std::future::Future<Output = Result<(), RuntimeError>> + Send {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
        fn restart(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<(), RuntimeError>> + Send {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
        fn remove(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<(), RuntimeError>> + Send {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
        fn inspect(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<crate::runtime::RuntimeSnapshot, RuntimeError>>
               + Send {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
        fn logs(
            &self,
            _container_id: &str,
            _tail: usize,
        ) -> impl std::future::Future<Output = Result<Vec<String>, RuntimeError>> + Send {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
        fn stats(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<crate::runtime::RuntimeStats, RuntimeError>> + Send
        {
            async { Err(RuntimeError::InvalidConfiguration("no daemon".into())) }
        }
    }

    #[tokio::test]
    async fn update_arm_pulls_before_mutating() {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pumpkin-update-test-{}", Uuid::new_v4()));
        let data = root.join("data");
        tokio::fs::create_dir_all(&data).await.expect("data dir");
        tokio::fs::write(data.join("level.dat"), b"fake world")
            .await
            .expect("seed file");
        let _env = BackupDirEnv::set(&root.join("backups"));
        seed_server(&pool, "srv-arm", 25565, data.to_string_lossy().as_ref()).await;

        let task_id = enqueue_update(&pool, None, "srv-arm", None, TARGET, None, true)
            .await
            .expect("enqueue");
        // Through the real dispatch arms: the worker takes its protected
        // backup, then fails at the pull (no daemon) — before stop/remove.
        task_runner::dispatch_task(&pool, &NoDaemon, &task_id)
            .await
            .expect("dispatch");
        let task = task_runner::get(&pool, &task_id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(task.state, task_runner::TaskState::Failed);
        assert_eq!(task.error_code.as_deref(), Some("IMAGE_PULL_FAILED"));

        // Nothing was swapped: current image untouched, no rollback target.
        let server = fetch_server(&pool, "srv-arm")
            .await
            .expect("fetch")
            .expect("row");
        assert_eq!(server.image_reference, "ghcr.io/pumpkin-mc/pumpkin:master");
        assert!(server.previous_image_reference.is_empty());
        assert!(server.container_id.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn rollback_failure_message_carries_warning() {
        let pool = test_pool().await;
        seed_server(&pool, "srv-rbfail", 25565, "/tmp/pumpkin-test-srv-1").await;
        sqlx::query(
            "UPDATE servers SET previous_image_reference = 'ghcr.io/pumpkin-mc/pumpkin:v1'
              WHERE id = 'srv-rbfail'",
        )
        .execute(&pool)
        .await
        .expect("seed previous");
        let task_id = enqueue_rollback(&pool, None, "srv-rbfail", None)
            .await
            .expect("enqueue");
        task_runner::dispatch_task(&pool, &NoDaemon, &task_id)
            .await
            .expect("dispatch");
        let task = task_runner::get(&pool, &task_id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(task.state, task_runner::TaskState::Failed);
        assert!(
            task.message.contains("one-way"),
            "failed rollback message must carry the one-way warning, got: {}",
            task.message
        );
    }

    // --- fix round: canonical backup path, configured dir, loud preconditions ---

    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Sets `PUMPKIN_MANAGER_BACKUP_DIR` for the test body; restores the env
    /// on drop. Serializes env-sensitive tests process-wide.
    struct BackupDirEnv {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl BackupDirEnv {
        fn set(dir: &std::path::Path) -> Self {
            let lock = ENV_GUARD.lock().expect("env guard");
            std::env::set_var("PUMPKIN_MANAGER_BACKUP_DIR", dir);
            Self { _lock: lock }
        }
    }

    impl Drop for BackupDirEnv {
        fn drop(&mut self) {
            std::env::remove_var("PUMPKIN_MANAGER_BACKUP_DIR");
        }
    }

    /// Pre-migration schema: `servers` WITHOUT the `0007_update` columns.
    async fn legacy_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        sqlx::query(
            "CREATE TABLE servers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                slug TEXT NOT NULL UNIQUE,
                image_reference TEXT NOT NULL DEFAULT '',
                image_digest TEXT NOT NULL DEFAULT '',
                container_id TEXT NOT NULL DEFAULT '',
                data_path TEXT NOT NULL DEFAULT '',
                game_port INTEGER NOT NULL UNIQUE,
                bind_address TEXT NOT NULL DEFAULT '0.0.0.0',
                desired_state TEXT NOT NULL DEFAULT 'stopped',
                observed_state TEXT NOT NULL DEFAULT 'created',
                deleted_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("servers table");
        sqlx::query(
            "CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                server_id TEXT REFERENCES servers(id) ON DELETE CASCADE,
                requested_by TEXT,
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
            )",
        )
        .execute(&pool)
        .await
        .expect("tasks table");
        sqlx::query(
            "CREATE TABLE backups (
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
                created_by TEXT,
                error_summary TEXT,
                protected INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .expect("backups table");
        sqlx::query(
            "CREATE TABLE audit_events (
                id TEXT PRIMARY KEY,
                actor_user_id TEXT,
                action TEXT NOT NULL,
                target_type TEXT NOT NULL DEFAULT '',
                target_id TEXT NOT NULL DEFAULT '',
                outcome TEXT NOT NULL DEFAULT 'ok',
                request_id TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                metadata TEXT NOT NULL DEFAULT '{}'
            )",
        )
        .execute(&pool)
        .await
        .expect("audit table");
        pool
    }

    #[tokio::test]
    async fn pre_update_backup_uses_canonical_path_and_configured_dir() {
        let pool = test_pool().await;
        let root =
            std::env::temp_dir().join(format!("pumpkin-update-canonical-{}", Uuid::new_v4()));
        let data = root.join("data");
        tokio::fs::create_dir_all(&data).await.expect("data dir");
        tokio::fs::write(data.join("level.dat"), b"fake world")
            .await
            .expect("seed file");
        let custom = root.join("custom-backups");
        let _env = BackupDirEnv::set(&custom);
        seed_server(&pool, "srv-canon", 25565, data.to_string_lossy().as_ref()).await;

        let task_id = enqueue_update(&pool, None, "srv-canon", None, TARGET, None, true)
            .await
            .expect("enqueue");
        // No daemon: the protected backup must succeed, then the pull fails.
        task_runner::dispatch_task(&pool, &NoDaemon, &task_id)
            .await
            .expect("dispatch");
        let task = task_runner::get(&pool, &task_id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(task.state, task_runner::TaskState::Failed);
        assert_eq!(task.error_code.as_deref(), Some("IMAGE_PULL_FAILED"));

        // The artifact lands in the CONFIGURED dir, not beside the data dir.
        let row: (String, String, i64) = sqlx::query_as(
            "SELECT storage_path, state, protected FROM backups
              WHERE server_id = 'srv-canon' ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .expect("backup row");
        assert_eq!(row.1, "completed");
        assert_eq!(row.2, 1);
        assert!(
            row.0.starts_with(custom.to_string_lossy().as_ref()),
            "pre-update artifact must live in the configured backup dir, got: {}",
            row.0
        );
        assert!(std::path::Path::new(&row.0).exists());
        // Canonical helper signature: it writes its own `backup.complete`
        // audit (the old inline capture never did).
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events WHERE action = 'backup.complete'",
        )
        .fetch_one(&pool)
        .await
        .expect("audit count");
        assert!(
            n >= 1,
            "canonical backup capture must audit backup.complete"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn pre_update_backup_failure_aborts_update() {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pumpkin-update-bakfail-{}", Uuid::new_v4()));
        let data = root.join("data");
        tokio::fs::create_dir_all(&data).await.expect("data dir");
        tokio::fs::write(data.join("level.dat"), b"fake world")
            .await
            .expect("seed file");
        // A FILE where the backup dir should be: the canonical storage
        // probe fails deterministically and the update must abort.
        let not_a_dir = root.join("not-a-dir");
        tokio::fs::write(&not_a_dir, b"in the way")
            .await
            .expect("blocker file");
        let _env = BackupDirEnv::set(&not_a_dir);
        seed_server(&pool, "srv-bakfail", 25565, data.to_string_lossy().as_ref()).await;

        let task_id = enqueue_update(&pool, None, "srv-bakfail", None, TARGET, None, true)
            .await
            .expect("enqueue");
        task_runner::dispatch_task(&pool, &NoDaemon, &task_id)
            .await
            .expect("dispatch");
        let task = task_runner::get(&pool, &task_id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(task.state, task_runner::TaskState::Failed);
        assert_eq!(task.error_code.as_deref(), Some("BACKUP_FAILED"));
        assert!(
            task.message.contains("backup storage unavailable"),
            "failure must come from the canonical storage probe, got: {}",
            task.message
        );
        // Aborted before pull/mutate: image refs and container untouched.
        let server = fetch_server(&pool, "srv-bakfail")
            .await
            .expect("fetch")
            .expect("row");
        assert_eq!(server.image_reference, "ghcr.io/pumpkin-mc/pumpkin:master");
        assert!(server.previous_image_reference.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn missing_migration_fails_loudly() {
        let pool = legacy_pool().await;
        seed_server(&pool, "srv-1", 25565, "/tmp/pumpkin-test-srv-1").await;
        // No silent fallback: without `0007_update` every entry point fails
        // with UPDATE_PRECONDITION instead of dropping the rollback target.
        let err = enqueue_update(&pool, None, "srv-1", None, TARGET, None, true)
            .await
            .expect_err("update without the migration must fail loudly");
        assert_eq!(err.code(), "UPDATE_PRECONDITION");
        let err = enqueue_rollback(&pool, None, "srv-1", None)
            .await
            .expect_err("rollback without the migration must fail loudly");
        assert_eq!(err.code(), "UPDATE_PRECONDITION");
        let err = preview_update(&pool, "srv-1", TARGET, None)
            .await
            .expect_err("preview without the migration must fail loudly");
        assert_eq!(err.code(), "UPDATE_PRECONDITION");
    }

    #[tokio::test]
    async fn swap_without_migration_fails_loudly() {
        let pool = legacy_pool().await;
        seed_server(&pool, "srv-1", 25565, "/tmp/pumpkin-test-srv-1").await;
        let err = swap_images(&pool, "srv-1", "prev", "", "cur", "")
            .await
            .expect_err("swap without the migration must fail loudly");
        assert_eq!(err.code(), "UPDATE_PRECONDITION");
        // And the row is untouched — no current-only silent write.
        let cur: String =
            sqlx::query_scalar("SELECT image_reference FROM servers WHERE id = 'srv-1'")
                .fetch_one(&pool)
                .await
                .expect("readback");
        assert_eq!(cur, "ghcr.io/pumpkin-mc/pumpkin:master");
    }
}
