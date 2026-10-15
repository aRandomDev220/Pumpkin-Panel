//! Backup/restore endpoints + background worker (Phase 5, Task 2).
//!
//! Contracts (wired by the controller in `main.rs`):
//! - `POST /servers/:id/backups {mode:"full", consistency:"live"|"stopped"}`
//!   → `202 {task_id, backup: {id, state}}`
//! - `GET /servers/:id/backups` → `{backups, total}`
//! - `GET /backups/:id` → backup object
//! - `GET /backups/:id/download` → tar.gz bytes
//! - `DELETE /backups/:id` → `204` (floor: protected / last verified → 409)
//! - `POST /backups/:id/restore {pre_restore_backup=true}` → `202 {task_id}`
//!
//! `desired_state` is never touched here: consistent backups stop/resume the
//! container directly and record whether it was running. A failed pre-restore
//! safety backup aborts the restore before anything is staged or swapped.
//!
//! Retention pruning applies the server's retention rule (most recently
//! updated enabled `backup_policies` row, else keep 7 / 30 days) through
//! Task 1's canonical decision fn — both routes and the scheduler go
//! through [`enqueue_backup`] / [`enqueue_restore`], which `tokio::spawn`
//! execution under a process-wide single-permit semaphore.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use sqlx::SqlitePool;
use tokio::sync::{Mutex, Semaphore};

use crate::{
    audit,
    auth::AuthUser,
    backup_service, file_service,
    runtime::{DockerAdapter, RuntimeAdapter, RuntimeError},
    task_runner, AppState,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Task type strings for backup/restore work (executed by our own spawned
/// worker, never submitted to the lifecycle [`task_runner::RunnerHandle`]).
const TASK_TYPE_BACKUP: &str = "backup";
const TASK_TYPE_RESTORE: &str = "restore";

/// Worker prune fallback when the server has no enabled retention policy
/// (mirrors the `backup_policies` column defaults in `0005_backups.sql`).
const DEFAULT_RETENTION_COUNT: usize = 7;
const DEFAULT_MAX_AGE_DAYS: u64 = 30;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Failures from the backup/restore surface. Carries its own HTTP mapping so
/// handlers and the future controller wiring agree on codes.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("server not found")]
    ServerNotFound,
    #[error("backup not found")]
    BackupNotFound,
    #[error("unsupported backup mode: {0}")]
    UnsupportedMode(String),
    #[error("invalid consistency: {0}")]
    InvalidConsistency(String),
    #[error("another operation is in flight for this server")]
    ServerBusy,
    #[error("server must be stopped for this operation")]
    ServerRunning,
    #[error("docker unavailable")]
    RuntimeUnavailable,
    #[error("backup storage unavailable: {0}")]
    Storage(String),
    #[error("insufficient disk space for backup")]
    NoSpace,
    #[error("backup archive is corrupt")]
    Corrupt,
    #[error("backup is protected from deletion")]
    Protected,
    #[error("backup was cancelled")]
    Cancelled,
    #[error("backup failed: {0}")]
    Failed(String),
    #[error("backup completed but restart failed: {0}")]
    ResumeFailed(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

impl From<task_runner::TaskError> for BackupError {
    fn from(e: task_runner::TaskError) -> Self {
        match e {
            task_runner::TaskError::Database(db) => Self::Db(db),
            other => Self::Failed(format!("task error: {other}")),
        }
    }
}

impl BackupError {
    /// Stable API error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ServerNotFound => "SERVER_NOT_FOUND",
            Self::BackupNotFound => "BACKUP_NOT_FOUND",
            Self::UnsupportedMode(_) | Self::InvalidConsistency(_) => "INVALID_CONFIGURATION",
            Self::ServerBusy => "SERVER_BUSY",
            Self::ServerRunning => "SERVER_RUNNING",
            Self::RuntimeUnavailable => "RUNTIME_UNAVAILABLE",
            Self::Storage(_) | Self::NoSpace | Self::Failed(_) | Self::ResumeFailed(_) => {
                "BACKUP_FAILED"
            }
            Self::Corrupt => "BACKUP_CORRUPT",
            Self::Protected => "BACKUP_PROTECTED",
            Self::Cancelled => "TASK_CANCELLED",
            Self::Db(_) => "INTERNAL",
        }
    }

    /// HTTP status for the failure.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::ServerNotFound | Self::BackupNotFound => StatusCode::NOT_FOUND,
            Self::UnsupportedMode(_) | Self::InvalidConsistency(_) | Self::Corrupt => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::ServerBusy | Self::ServerRunning | Self::Protected | Self::Cancelled => {
                StatusCode::CONFLICT
            }
            Self::RuntimeUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Storage(_) | Self::NoSpace | Self::Failed(_) | Self::ResumeFailed(_) => {
                StatusCode::BAD_GATEWAY
            }
            Self::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
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

fn backup_error(e: BackupError) -> Response {
    error(e.status(), e.code(), &e.to_string())
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct ServerRow {
    id: String,
    data_path: String,
    container_id: String,
    desired_state: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct BackupRow {
    id: String,
    server_id: String,
    backup_type: String,
    state: String,
    storage_path: String,
    size_bytes: i64,
    checksum_algorithm: String,
    checksum: String,
    consistency_method: String,
    created_at: String,
    completed_at: Option<String>,
    created_by: Option<String>,
    error_summary: Option<String>,
    protected: i64,
}

/// One backup row projected onto Task 1's retention-decision input.
#[derive(Debug, Clone, sqlx::FromRow)]
struct RetentionRow {
    id: String,
    state: String,
    completed_at: Option<String>,
    protected: i64,
    storage_path: String,
}

const BACKUP_COLUMNS: &str = "id, server_id, type AS backup_type, state, storage_path, \
    size_bytes, checksum_algorithm, checksum, consistency_method, created_at, \
    completed_at, created_by, error_summary, protected";

async fn fetch_server(pool: &SqlitePool, id: &str) -> Result<Option<ServerRow>, sqlx::Error> {
    sqlx::query_as::<_, ServerRow>(
        "SELECT id, data_path, container_id, desired_state
         FROM servers WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

async fn fetch_backup(pool: &SqlitePool, id: &str) -> Result<Option<BackupRow>, sqlx::Error> {
    sqlx::query_as::<_, BackupRow>(&format!(
        "SELECT {BACKUP_COLUMNS} FROM backups WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await
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

fn backup_json(b: &BackupRow) -> serde_json::Value {
    serde_json::json!({
        "id": b.id,
        "server_id": b.server_id,
        "type": b.backup_type,
        "state": b.state,
        "storage_path": b.storage_path,
        "size_bytes": b.size_bytes,
        "checksum_algorithm": b.checksum_algorithm,
        "checksum": b.checksum,
        "consistency_method": b.consistency_method,
        "created_at": b.created_at,
        "completed_at": b.completed_at,
        "created_by": b.created_by,
        "error_summary": b.error_summary,
        "protected": b.protected != 0,
    })
}

// ---------------------------------------------------------------------------
// Roots, hashing, space
// ---------------------------------------------------------------------------

/// Best-effort boot cleanup: remove `.tmp-*` staging entries predating this
/// boot from the backup root, plus `.tmp-*`/`.recovery-*` crash leftovers
/// beside every live server's data dir (restore stages `.tmp-restore-<id>`
/// and parks `.recovery-<ts>` in the server root). In-progress backup *rows*
/// are left alone (`requeue_interrupted` already fails their tasks; the rows
/// stay as evidence).
pub async fn startup_cleanup(pool: &SqlitePool, backup_root: &Path) {
    clean_staging_dir(backup_root, false);
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT data_path FROM servers WHERE deleted_at IS NULL")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    let mut parents = HashSet::new();
    for (data_path,) in rows {
        if let Some(parent) = parent_of(&data_path) {
            parents.insert(parent);
        }
    }
    for parent in parents {
        clean_staging_dir(&parent, true);
    }
}

/// Remove stale staging entries from `dir`: always `.tmp-*`, plus
/// `.recovery-*` when `with_recovery` (server data-dir parents only). An
/// entry counts as stale when its mtime predates this boot (`<= now` at
/// cleanup time); live data files never match either prefix.
fn clean_staging_dir(dir: &Path, with_recovery: bool) {
    let now = std::time::SystemTime::now();
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e,
                "backup cleanup: cannot list dir");
            return;
        }
    };
    let mut removed = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let staging = name.starts_with(".tmp-");
        let recovery = with_recovery && name.starts_with(".recovery-");
        if !staging && !recovery {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| t <= now)
            .unwrap_or(true);
        if !stale {
            continue;
        }
        let path = entry.path();
        let ok = if path.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if ok {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, path = %dir.display(),
            "backup cleanup removed stale staging entries");
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// SHA-256 hex of a file (blocking: call from `spawn_blocking` or tests).
fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
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

/// Free bytes on the filesystem holding `path`, via the longest matching
/// mount point. `None` when nothing matches (caller proceeds optimistically).
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

/// Fully decode a tar.gz in memory, draining every regular file. Proves the
/// archive round-trips before we call a backup verified.
fn verify_archive_bytes(data: &[u8]) -> Result<usize, String> {
    use std::io::Read as _;
    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|e| e.to_string())?;
    let mut count = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if entry.header().entry_type().is_file() {
            let mut sink = Vec::new();
            entry.read_to_end(&mut sink).map_err(|e| e.to_string())?;
        }
        count += 1;
    }
    Ok(count)
}

/// `create_dir_all` plus a write-probe, so unwritable storage fails fast at
/// enqueue time instead of mid-worker.
async fn ensure_writable_dir(dir: &Path) -> Result<(), String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("cannot create backup dir {}: {e}", dir.display()))?;
    let probe = dir.join(format!(".write-probe-{}", uuid::Uuid::new_v4()));
    tokio::fs::write(&probe, b"probe")
        .await
        .map_err(|e| format!("backup dir {} is not writable: {e}", dir.display()))?;
    let _ = tokio::fs::remove_file(&probe).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Worker Primitives (semaphore, op locks, cancel, audits)
// ---------------------------------------------------------------------------

static BACKUP_SEMAPHORE: OnceLock<Semaphore> = OnceLock::new();

fn semaphore() -> &'static Semaphore {
    BACKUP_SEMAPHORE.get_or_init(|| Semaphore::new(1))
}

/// Try to claim the per-server op lock on the shared [`AppState::op_locks`].
/// `false` = conflicting op in flight (same pattern as `routes_servers`).
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

async fn cancel_requested(pool: &SqlitePool, task_id: &str) -> bool {
    matches!(
        task_runner::get(pool, task_id).await,
        Ok(Some(ref t)) if t.state == task_runner::TaskState::CancelRequested
    )
}

fn truncate500(s: &str) -> String {
    s.chars().take(500).collect()
}

async fn fail_backup_row(pool: &SqlitePool, backup_id: &str, summary: &str) {
    let _ = sqlx::query("UPDATE backups SET state = 'failed', error_summary = ? WHERE id = ?")
        .bind(truncate500(summary))
        .bind(backup_id)
        .execute(pool)
        .await;
}

async fn fail_backup(pool: &SqlitePool, task_id: &str, backup_id: &str, code: &str, summary: &str) {
    fail_backup_row(pool, backup_id, summary).await;
    let _ = task_runner::mark_failed(pool, task_id, code, &truncate500(summary)).await;
    audit::record(
        pool,
        None,
        "backup.fail",
        "backup",
        backup_id,
        "failed",
        &audit::request_id(),
        serde_json::json!({ "task_id": task_id, "error_code": code }),
    )
    .await;
}

/// Heuristic for "Docker is unreachable" (mirrors `routes_servers`).
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

fn parent_of(data_path: &str) -> Option<PathBuf> {
    let path = Path::new(data_path);
    if data_path.is_empty() {
        return None;
    }
    path.parent().map(Path::to_path_buf)
}

fn nanos_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Shared backup capture (used by the backup worker AND the pre-restore
// safety backup). The caller holds the server op lock and — for the spawned
// workers — the backup semaphore permit.
// ---------------------------------------------------------------------------

struct BackupOutcome {
    storage_path: String,
    size_bytes: i64,
    checksum: String,
    archive_entries: usize,
}

/// Capture a verified `.tar.gz` of the server data dir. Stops the container
/// first when `consistency == "stopped"` and it is running (recording the
/// need to resume in the return value); `desired_state` is never modified.
#[allow(clippy::too_many_arguments)]
async fn run_backup_capture(
    pool: &SqlitePool,
    adapter: Option<&DockerAdapter>,
    backup_dir: &Path,
    backup_id: &str,
    server: &ServerRow,
    consistency: &str,
    cancel_task_id: &str,
    actor: Option<&str>,
) -> Result<(BackupOutcome, Option<String>), BackupError> {
    let server_root = parent_of(&server.data_path)
        .ok_or_else(|| BackupError::Failed("server has no data path".into()))?;
    // Single source of truth: the configured dir, threaded from `AppState`.
    let backup_root = backup_dir.to_path_buf();
    ensure_writable_dir(&backup_root)
        .await
        .map_err(BackupError::Storage)?;
    let data_dir = if server.data_path.is_empty() {
        server_root.join(&server.id)
    } else {
        PathBuf::from(&server.data_path)
    };
    if data_dir == server_root {
        return Err(BackupError::Failed(
            "refusing to back up the server root itself".into(),
        ));
    }
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| BackupError::Failed(format!("cannot open server data dir: {e}")))?;

    // Pre-check: refuse when the disk cannot hold ~2x the data estimate.
    let estimate = tokio::task::spawn_blocking({
        let dir = data_dir.clone();
        move || dir_size_bytes(&dir)
    })
    .await
    .map_err(|_| BackupError::Failed("size estimate task failed".into()))?;
    if estimate > 0 {
        if let Some(free) = free_bytes_for(&backup_root) {
            if free < estimate.saturating_mul(2) {
                return Err(BackupError::NoSpace);
            }
        }
    }

    if cancel_requested(pool, cancel_task_id).await {
        return Err(BackupError::Cancelled);
    }

    // Consistent snapshot: stop a running container directly (desired_state
    // untouched), remembering to resume it afterwards.
    let mut resume: Option<String> = None;
    if consistency == "stopped" && !server.container_id.is_empty() {
        let Some(a) = adapter else {
            return Err(BackupError::RuntimeUnavailable);
        };
        match a.inspect(&server.container_id).await {
            Ok(snap) => {
                if snap.status == "running" {
                    a.stop(&server.container_id, task_runner::TASK_STOP_TIMEOUT_SECS)
                        .await
                        .map_err(|e| {
                            if is_daemon_down(&e) {
                                BackupError::RuntimeUnavailable
                            } else {
                                BackupError::Failed(format!(
                                    "failed to stop container for consistent backup: {e}"
                                ))
                            }
                        })?;
                    resume = Some(server.container_id.clone());
                }
            }
            Err(RuntimeError::NotFound(_)) => {}
            Err(e) if is_daemon_down(&e) => return Err(BackupError::RuntimeUnavailable),
            Err(e) => {
                return Err(BackupError::Failed(format!(
                    "failed to inspect container: {e}"
                )));
            }
        }
    }

    // Tar the data dir (blocking: gzip-bound), stage as `.part`, rename.
    let bytes = tokio::task::spawn_blocking({
        let dir = data_dir.clone();
        move || file_service::export_tar_gz(&dir, &[""])
    })
    .await
    .map_err(|_| BackupError::Failed("backup export task failed".into()))?
    .map_err(|e| BackupError::Failed(format!("backup export failed: {e}")))?;
    let part_path = backup_root.join(format!("{backup_id}.tar.gz.part"));
    let final_path = backup_root.join(format!("{backup_id}.tar.gz"));
    tokio::fs::write(&part_path, &bytes)
        .await
        .map_err(|e| BackupError::Failed(format!("failed to write backup archive: {e}")))?;
    tokio::fs::rename(&part_path, &final_path)
        .await
        .map_err(|e| BackupError::Failed(format!("failed to publish backup archive: {e}")))?;

    // Verify from disk: checksum + full archive re-list.
    let final_path_clone = final_path.clone();
    let (checksum, size_bytes, archive_entries) =
        tokio::task::spawn_blocking(move || -> Result<(String, i64, usize), String> {
            let data = std::fs::read(&final_path_clone).map_err(|e| e.to_string())?;
            let entries = verify_archive_bytes(&data)?;
            use sha2::{Digest as _, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&data);
            Ok((hex_encode(&hasher.finalize()), data.len() as i64, entries))
        })
        .await
        .map_err(|_| BackupError::Failed("backup verify task failed".into()))?
        .map_err(BackupError::Failed)?;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let _ = sqlx::query(
        "UPDATE backups SET state = 'completed', storage_path = ?, size_bytes = ?,
                checksum_algorithm = 'sha256', checksum = ?, completed_at = ?
         WHERE id = ?",
    )
    .bind(final_path.to_string_lossy().into_owned())
    .bind(size_bytes)
    .bind(&checksum)
    .bind(&now)
    .bind(backup_id)
    .execute(pool)
    .await;
    audit::record(
        pool,
        actor,
        "backup.complete",
        "backup",
        backup_id,
        "ok",
        &audit::request_id(),
        serde_json::json!({
            "server_id": server.id,
            "size_bytes": size_bytes,
            "entries": archive_entries,
            "consistency": consistency,
        }),
    )
    .await;
    Ok((
        BackupOutcome {
            storage_path: final_path.to_string_lossy().into_owned(),
            size_bytes,
            checksum,
            archive_entries,
        },
        resume,
    ))
}

// ---------------------------------------------------------------------------
// Backup worker
// ---------------------------------------------------------------------------

/// The server's retention rule, shared by the worker prune and the
/// retention preview (scheduler): the most recently updated ENABLED
/// `backup_policies` row, else the table defaults
/// (`retention_count=7, max_age_days=30`). One query, one rule — preview can
/// never promise deletions the worker would not perform.
pub(crate) async fn retention_rule(pool: &SqlitePool, server_id: &str) -> (usize, u64) {
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT retention_count, max_age_days FROM backup_policies
         WHERE server_id = ? AND enabled != 0
         ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    match row {
        Some((keep, age)) => (keep.max(0) as usize, age.max(0) as u64),
        None => (DEFAULT_RETENTION_COUNT, DEFAULT_MAX_AGE_DAYS),
    }
}
/// Retention prune via Task 1's canonical
/// [`backup_service::select_for_deletion`]: backup rows newest-first mapped
/// to [`backup_service::RetentionInput`], with the server's retention rule
/// (see [`retention_rule`]). Protected rows consume keep slots and
/// in-progress rows are never listed, per the canonical semantics. The
/// executor (file + row deletion, per-row audit) stays here. Best-effort —
/// never fails the task.
async fn prune_retention(pool: &SqlitePool, server_id: &str, actor: Option<&str>) {
    let rows = sqlx::query_as::<_, RetentionRow>(
        "SELECT id, state, completed_at, protected, storage_path FROM backups
         WHERE server_id = ? ORDER BY created_at DESC",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let inputs: Vec<backup_service::RetentionInput> = rows
        .iter()
        .map(|r| backup_service::RetentionInput {
            id: r.id.clone(),
            completed_at: r.completed_at.clone(),
            verified: r.state == "completed",
            protected: r.protected != 0,
            in_progress: r.state == "in_progress",
        })
        .collect();
    let (keep, max_age_days) = retention_rule(pool, server_id).await;
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let delete = backup_service::select_for_deletion(&inputs, keep, max_age_days, now_unix);
    if delete.is_empty() {
        return;
    }
    for id in &delete {
        if let Some(row) = rows.iter().find(|r| &r.id == id) {
            if !row.storage_path.is_empty() {
                let _ = tokio::fs::remove_file(&row.storage_path).await;
            }
        }
        let _ = sqlx::query("DELETE FROM backups WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await;
        audit::record(
            pool,
            actor,
            "backup.prune",
            "backup",
            id,
            "ok",
            &audit::request_id(),
            serde_json::json!({
                "server_id": server_id,
                "reason": "retention",
                "retention_count": keep,
                "max_age_days": max_age_days,
            }),
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_backup(
    pool: &SqlitePool,
    adapter: Option<&DockerAdapter>,
    backup_dir: &Path,
    task_id: &str,
    backup_id: &str,
    server_id: &str,
    consistency: &str,
    actor: Option<&str>,
) {
    if task_runner::mark_running(pool, task_id, "backup in progress")
        .await
        .is_err()
    {
        return;
    }
    // Cross-operation safety comes from the shared `AppState::op_locks`
    // (held by lifecycle handlers for their synchronous section) plus the
    // `conflicting_task` check in `enqueue_backup`; the worker itself holds
    // no lock, mirroring the lifecycle runner.
    let outcome = execute_backup_inner(
        pool,
        adapter,
        backup_dir,
        task_id,
        backup_id,
        server_id,
        consistency,
        actor,
    )
    .await;

    match outcome {
        Ok(outcome) => {
            // Cooperative cancel before the prune: the backup itself is done
            // and verified; only the retention sweep is skipped.
            if cancel_requested(pool, task_id).await {
                let _ = task_runner::mark_cancelled(pool, task_id).await;
                audit::record(
                    pool,
                    actor,
                    "backup.cancel",
                    "backup",
                    backup_id,
                    "cancelled",
                    &audit::request_id(),
                    serde_json::json!({ "task_id": task_id }),
                )
                .await;
                return;
            }
            prune_retention(pool, server_id, actor).await;
            audit::record(
                pool,
                actor,
                "backup.complete",
                "backup",
                backup_id,
                "ok",
                &audit::request_id(),
                serde_json::json!({
                    "server_id": server_id,
                    "storage_path": outcome.storage_path,
                    "size_bytes": outcome.size_bytes,
                    "archive_entries": outcome.archive_entries,
                }),
            )
            .await;
            let _ = task_runner::mark_succeeded(pool, task_id, "backup completed").await;
        }
        Err(BackupError::Cancelled) => {
            let part = backup_part_path(backup_dir, backup_id);
            let _ = tokio::fs::remove_file(&part).await;
            fail_backup_row(pool, backup_id, "cancelled by user").await;
            let _ = task_runner::mark_cancelled(pool, task_id).await;
            audit::record(
                pool,
                actor,
                "backup.cancel",
                "backup",
                backup_id,
                "cancelled",
                &audit::request_id(),
                serde_json::json!({ "task_id": task_id }),
            )
            .await;
        }
        Err(BackupError::ResumeFailed(msg)) => {
            // The backup row is `completed` and verified; only the task
            // reports the restart failure, with both facts recorded.
            let _ =
                task_runner::mark_failed(pool, task_id, "RESTART_FAILED", &truncate500(&msg)).await;
            audit::record(
                pool,
                actor,
                "backup.resume",
                "backup",
                backup_id,
                "failed",
                &audit::request_id(),
                serde_json::json!({ "task_id": task_id, "message": truncate500(&msg) }),
            )
            .await;
        }
        Err(e) => {
            fail_backup(pool, task_id, backup_id, e.code(), &e.to_string()).await;
        }
    }
}

fn backup_part_path(backup_dir: &Path, backup_id: &str) -> PathBuf {
    backup_dir.join(format!("{backup_id}.tar.gz.part"))
}

#[allow(clippy::too_many_arguments)]
async fn execute_backup_inner(
    pool: &SqlitePool,
    adapter: Option<&DockerAdapter>,
    backup_dir: &Path,
    task_id: &str,
    backup_id: &str,
    server_id: &str,
    consistency: &str,
    actor: Option<&str>,
) -> Result<BackupOutcome, BackupError> {
    let server = fetch_server(pool, server_id)
        .await?
        .ok_or(BackupError::ServerNotFound)?;
    let (outcome, resume) = run_backup_capture(
        pool,
        adapter,
        backup_dir,
        backup_id,
        &server,
        consistency,
        task_id,
        actor,
    )
    .await?;
    // Resume the container we stopped. `desired_state` stays untouched; a
    // restart failure fails the task but keeps the verified backup row.
    if let Some(cid) = resume {
        let Some(a) = adapter else {
            return Err(BackupError::ResumeFailed(
                "runtime detached during backup; container left stopped".into(),
            ));
        };
        if let Err(e) = a.start(&cid).await {
            return Err(BackupError::ResumeFailed(format!(
                "backup {backup_id} completed ({} bytes, sha256 {}); \
                 failed to restart container: {e}",
                outcome.size_bytes, outcome.checksum
            )));
        }
        audit::record(
            pool,
            actor,
            "backup.resume",
            "backup",
            backup_id,
            "ok",
            &audit::request_id(),
            serde_json::json!({ "server_id": server.id }),
        )
        .await;
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Restore worker
// ---------------------------------------------------------------------------

/// Re-verify a completed backup row against the file on disk. Anything off —
/// missing row state, missing file, checksum mismatch — is `BACKUP_CORRUPT`.
async fn verify_backup_file(backup: &BackupRow) -> Result<(), BackupError> {
    if backup.state != "completed" || backup.storage_path.is_empty() || backup.checksum.is_empty() {
        return Err(BackupError::Corrupt);
    }
    let path = PathBuf::from(&backup.storage_path);
    let expected = backup.checksum.clone();
    let actual = tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(|_| BackupError::Failed("verify task failed".into()))?
        .map_err(|_| BackupError::Corrupt)?;
    if actual != expected {
        return Err(BackupError::Corrupt);
    }
    Ok(())
}

async fn execute_restore(
    pool: &SqlitePool,
    adapter: Option<&DockerAdapter>,
    backup_dir: &Path,
    task_id: &str,
    backup_id: &str,
    pre_restore_backup: bool,
    actor: Option<&str>,
) {
    if task_runner::mark_running(pool, task_id, "restore in progress")
        .await
        .is_err()
    {
        return;
    }
    let backup = match fetch_backup(pool, backup_id).await {
        Ok(Some(b)) => b,
        _ => {
            let _ =
                task_runner::mark_failed(pool, task_id, "BACKUP_NOT_FOUND", "backup row vanished")
                    .await;
            return;
        }
    };
    let result = execute_restore_inner(
        pool,
        adapter,
        backup_dir,
        task_id,
        &backup,
        pre_restore_backup,
        actor,
    )
    .await;

    match result {
        Ok(msg) => {
            let _ = task_runner::mark_succeeded(pool, task_id, &msg).await;
        }
        Err(BackupError::Cancelled) => {
            let _ = task_runner::mark_cancelled(pool, task_id).await;
            audit::record(
                pool,
                actor,
                "restore.cancel",
                "backup",
                backup_id,
                "cancelled",
                &audit::request_id(),
                serde_json::json!({ "task_id": task_id }),
            )
            .await;
        }
        Err(e) => {
            let _ = task_runner::mark_failed(pool, task_id, e.code(), &truncate500(&e.to_string()))
                .await;
            audit::record(
                pool,
                actor,
                "restore.fail",
                "backup",
                backup_id,
                "failed",
                &audit::request_id(),
                serde_json::json!({ "task_id": task_id, "error_code": e.code() }),
            )
            .await;
        }
    }
}

async fn execute_restore_inner(
    pool: &SqlitePool,
    adapter: Option<&DockerAdapter>,
    backup_dir: &Path,
    task_id: &str,
    backup: &BackupRow,
    pre_restore_backup: bool,
    actor: Option<&str>,
) -> Result<String, BackupError> {
    let server = fetch_server(pool, &backup.server_id)
        .await?
        .ok_or(BackupError::ServerNotFound)?;
    // Stopped means stopped: route-level `desired_state` gate, re-checked
    // here, plus the observed container state when a runtime is attached.
    if server.desired_state == "running" {
        return Err(BackupError::ServerRunning);
    }
    if let Some(a) = adapter {
        if !server.container_id.is_empty() {
            match a.inspect(&server.container_id).await {
                Ok(snap) if snap.status == "running" => {
                    return Err(BackupError::ServerRunning);
                }
                Ok(_) => {}
                Err(RuntimeError::NotFound(_)) => {}
                Err(e) if is_daemon_down(&e) => return Err(BackupError::RuntimeUnavailable),
                Err(e) => {
                    return Err(BackupError::Failed(format!(
                        "failed to inspect container: {e}"
                    )));
                }
            }
        }
    }
    if cancel_requested(pool, task_id).await {
        return Err(BackupError::Cancelled);
    }

    // Re-fetch + re-verify inside the worker: the archive on disk is the
    // source of truth, and nothing is written before it checks out.
    let backup = fetch_backup(pool, &backup.id)
        .await?
        .ok_or(BackupError::BackupNotFound)?;
    verify_backup_file(&backup).await?;
    let archive_bytes = tokio::fs::read(&backup.storage_path)
        .await
        .map_err(|_| BackupError::Corrupt)?;

    // Auto pre-restore safety backup (protected from retention). Its failure
    // ABORTS the restore — live data is never touched on this path.
    let mut pre_id: Option<String> = None;
    if pre_restore_backup {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO backups
                (id, server_id, type, state, consistency_method, protected, created_by)
             VALUES (?, ?, 'full', 'in_progress', 'stopped', 1, ?)",
        )
        .bind(&id)
        .bind(&server.id)
        .bind(actor)
        .execute(pool)
        .await?;
        audit::record(
            pool,
            actor,
            "restore.pre_backup",
            "backup",
            &id,
            "started",
            &audit::request_id(),
            serde_json::json!({ "server_id": server.id, "restore_backup_id": backup.id }),
        )
        .await;
        match run_backup_capture(
            pool, adapter, backup_dir, &id, &server, "stopped", task_id, actor,
        )
        .await
        {
            Ok(_) => {
                audit::record(
                    pool,
                    actor,
                    "restore.pre_backup",
                    "backup",
                    &id,
                    "ok",
                    &audit::request_id(),
                    serde_json::json!({ "server_id": server.id }),
                )
                .await;
                pre_id = Some(id);
            }
            Err(e) => {
                fail_backup_row(pool, &id, &e.to_string()).await;
                audit::record(
                    pool,
                    actor,
                    "restore.pre_backup",
                    "backup",
                    &id,
                    "failed",
                    &audit::request_id(),
                    serde_json::json!({ "server_id": server.id, "error": e.to_string() }),
                )
                .await;
                return Err(BackupError::Failed(format!(
                    "pre-restore backup failed, restore aborted: {e}"
                )));
            }
        }
    }

    if cancel_requested(pool, task_id).await {
        return Err(BackupError::Cancelled);
    }

    // Stage-extract into a sibling temp dir, validated before any swap.
    let server_root = parent_of(&server.data_path)
        .ok_or_else(|| BackupError::Failed("server has no data path".into()))?;
    let data_dir = if server.data_path.is_empty() {
        server_root.join(&server.id)
    } else {
        PathBuf::from(&server.data_path)
    };
    if data_dir == server_root {
        return Err(BackupError::Failed(
            "refusing to restore over the server root itself".into(),
        ));
    }
    let staging = server_root.join(format!(".tmp-restore-{}", backup.id));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| BackupError::Failed(format!("cannot stage restore: {e}")))?;
    let staged = tokio::task::spawn_blocking({
        let staging = staging.clone();
        let limits = file_service::DEFAULT_ARCHIVE_LIMITS;
        move || file_service::import_tar_gz(&staging, "", &archive_bytes, &limits)
    })
    .await
    .map_err(|_| BackupError::Failed("restore extract task failed".into()))?;
    let staged_count = match staged {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(BackupError::Failed(format!(
                "restore archive failed validation: {e}"
            )));
        }
    };
    // Validate the listing: the staged tree must read back.
    let listed = std::fs::read_dir(&staging)
        .map(|rd| rd.count())
        .map_err(|e| BackupError::Failed(format!("staged restore unreadable: {e}")))?;
    let _ = listed;
    audit::record(
        pool,
        actor,
        "restore.stage",
        "backup",
        &backup.id,
        "ok",
        &audit::request_id(),
        serde_json::json!({ "server_id": server.id, "files": staged_count }),
    )
    .await;

    if cancel_requested(pool, task_id).await {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(BackupError::Cancelled);
    }

    // Swap: current data aside, staged tree in. Recovery is deleted on
    // success and KEPT (with its path reported) on failure.
    let recovery = server_root.join(format!(".recovery-{}", nanos_now()));
    let had_data = data_dir.exists();
    if had_data {
        std::fs::rename(&data_dir, &recovery).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            BackupError::Failed(format!("restore swap failed, current data untouched: {e}"))
        })?;
    }
    if let Err(e) = std::fs::rename(&staging, &data_dir) {
        let mut msg = format!("restore swap failed: {e}");
        if had_data {
            match std::fs::rename(&recovery, &data_dir) {
                Ok(()) => msg.push_str("; current data rolled back, recovery restored"),
                Err(e2) => msg.push_str(&format!(
                    "; ROLLBACK FAILED, prior data preserved at {}: {e2}",
                    recovery.display()
                )),
            }
        }
        return Err(BackupError::Failed(msg));
    }
    if had_data {
        let _ = std::fs::remove_dir_all(&recovery);
    }
    audit::record(
        pool,
        actor,
        "restore.complete",
        "backup",
        &backup.id,
        "ok",
        &audit::request_id(),
        serde_json::json!({
            "server_id": server.id,
            "files": staged_count,
            "pre_restore_backup_id": pre_id,
        }),
    )
    .await;
    Ok(format!(
        "restore completed ({} files{} )",
        staged_count,
        pre_id
            .map(|id| format!(", pre-restore backup {id}"))
            .unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// Single entry points (routes AND scheduler)
// ---------------------------------------------------------------------------

/// Enqueue a full backup of `server_id` and spawn its execution under the
/// backup semaphore. Returns `(task_id, backup_id)`.
pub async fn enqueue_backup(
    pool: &SqlitePool,
    adapter: Option<&DockerAdapter>,
    backup_dir: &Path,
    server_id: &str,
    requested_by: Option<&str>,
    consistency: &str,
) -> Result<(String, String), BackupError> {
    if !matches!(consistency, "live" | "stopped") {
        return Err(BackupError::InvalidConsistency(consistency.to_string()));
    }
    let server = fetch_server(pool, server_id)
        .await?
        .ok_or(BackupError::ServerNotFound)?;
    if conflicting_task(pool, server_id).await {
        return Err(BackupError::ServerBusy);
    }
    // Fail fast on unwritable storage, before any row exists.
    ensure_writable_dir(backup_dir)
        .await
        .map_err(BackupError::Storage)?;
    if parent_of(&server.data_path).is_none() {
        return Err(BackupError::Failed("server has no data path".into()));
    }

    let task = task_runner::enqueue(pool, TASK_TYPE_BACKUP, Some(server_id), requested_by).await?;
    let backup_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO backups
            (id, server_id, type, state, consistency_method, created_by)
         VALUES (?, ?, 'full', 'in_progress', ?, ?)",
    )
    .bind(&backup_id)
    .bind(server_id)
    .bind(consistency)
    .bind(requested_by)
    .execute(pool)
    .await?;

    let pool_owned = pool.clone();
    let adapter_owned = adapter.cloned();
    let backup_dir_owned = backup_dir.to_path_buf();
    let server_owned = server_id.to_string();
    let task_owned = task.id.clone();
    let backup_owned = backup_id.clone();
    let consistency_owned = consistency.to_string();
    let actor_owned = requested_by.map(str::to_string);
    tokio::spawn(async move {
        let Ok(_permit) = semaphore().acquire().await else {
            fail_backup(
                &pool_owned,
                &task_owned,
                &backup_owned,
                "BACKUP_FAILED",
                "backup worker unavailable",
            )
            .await;
            return;
        };
        execute_backup(
            &pool_owned,
            adapter_owned.as_ref(),
            &backup_dir_owned,
            &task_owned,
            &backup_owned,
            &server_owned,
            &consistency_owned,
            actor_owned.as_deref(),
        )
        .await;
    });
    Ok((task.id, backup_id))
}

/// Enqueue a restore of `backup_id` and spawn its execution. The backup row,
/// its server, the stopped gate, the archive checksum, and storage
/// writability are all verified synchronously, so acceptance (`202`) means
/// the restore is genuinely runnable.
pub async fn enqueue_restore(
    pool: &SqlitePool,
    adapter: Option<&DockerAdapter>,
    backup_dir: &Path,
    backup_id: &str,
    requested_by: Option<&str>,
    pre_restore_backup: bool,
) -> Result<String, BackupError> {
    let backup = fetch_backup(pool, backup_id)
        .await?
        .ok_or(BackupError::BackupNotFound)?;
    let server = fetch_server(pool, &backup.server_id)
        .await?
        .ok_or(BackupError::ServerNotFound)?;
    if server.desired_state == "running" {
        return Err(BackupError::ServerRunning);
    }
    if conflicting_task(pool, &server.id).await {
        return Err(BackupError::ServerBusy);
    }
    // Synchronous guards: corrupt archives and unwritable storage fail here,
    // before any task exists and before live data is touched.
    verify_backup_file(&backup).await?;
    ensure_writable_dir(backup_dir)
        .await
        .map_err(BackupError::Storage)?;
    if parent_of(&server.data_path).is_none() {
        return Err(BackupError::Failed("server has no data path".into()));
    }

    let task =
        task_runner::enqueue(pool, TASK_TYPE_RESTORE, Some(&server.id), requested_by).await?;

    let pool_owned = pool.clone();
    let adapter_owned = adapter.cloned();
    let backup_dir_owned = backup_dir.to_path_buf();
    let task_owned = task.id.clone();
    let backup_owned = backup_id.to_string();
    let actor_owned = requested_by.map(str::to_string);
    tokio::spawn(async move {
        let Ok(_permit) = semaphore().acquire().await else {
            let _ = task_runner::mark_failed(
                &pool_owned,
                &task_owned,
                "BACKUP_FAILED",
                "backup worker unavailable",
            )
            .await;
            return;
        };
        execute_restore(
            &pool_owned,
            adapter_owned.as_ref(),
            &backup_dir_owned,
            &task_owned,
            &backup_owned,
            pre_restore_backup,
            actor_owned.as_deref(),
        )
        .await;
    });
    Ok(task.id)
}

// ---------------------------------------------------------------------------
// Handlers (wired + permission-gated by the controller in `main.rs`)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateBackupRequest {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub consistency: Option<String>,
}

/// POST /api/v1/servers/:id/backups — enqueue a backup, `202`.
pub async fn create_backup(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<CreateBackupRequest>,
) -> Response {
    let request_id = audit::request_id();
    let mode = body.mode.unwrap_or_else(|| "full".to_string());
    if mode != "full" {
        return backup_error(BackupError::UnsupportedMode(mode));
    }
    let consistency = body.consistency.unwrap_or_else(|| "stopped".to_string());
    // Shared op lock (same domain as lifecycle ops): fail fast when another
    // operation holds the server, mirroring `routes_servers`.
    if !try_lock(&state.op_locks, &id).await {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another operation is in flight for this server",
        );
    }
    let outcome = enqueue_backup(
        &state.pool,
        state.adapter.as_deref(),
        &state.backup_dir,
        &id,
        Some(&user.id),
        &consistency,
    )
    .await;
    unlock(&state.op_locks, &id).await;
    match outcome {
        Ok((task_id, backup_id)) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "backup.create",
                "backup",
                &backup_id,
                "accepted",
                &request_id,
                serde_json::json!({
                    "server_id": id,
                    "task_id": task_id,
                    "consistency": consistency,
                }),
            )
            .await;
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "task_id": task_id,
                    "backup": { "id": backup_id, "state": "in_progress" },
                })),
            )
                .into_response()
        }
        Err(e) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "backup.create",
                "server",
                &id,
                "failed",
                &request_id,
                serde_json::json!({ "error_code": e.code() }),
            )
            .await;
            backup_error(e)
        }
    }
}

/// GET /api/v1/servers/:id/backups — newest first.
pub async fn list_backups(
    State(state): State<AppState>,
    _user: AuthUser,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if fetch_server(&state.pool, &id)
        .await
        .unwrap_or(None)
        .is_none()
    {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    let rows = sqlx::query_as::<_, BackupRow>(&format!(
        "SELECT {BACKUP_COLUMNS} FROM backups WHERE server_id = ? ORDER BY created_at DESC"
    ))
    .bind(&id)
    .fetch_all(&state.pool)
    .await;
    match rows {
        Ok(rows) => {
            let backups: Vec<serde_json::Value> = rows.iter().map(backup_json).collect();
            let total = backups.len();
            Json(serde_json::json!({ "backups": backups, "total": total })).into_response()
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to list backups",
        ),
    }
}

/// GET /api/v1/backups/:id — one backup row.
pub async fn get_backup(
    State(state): State<AppState>,
    _user: AuthUser,
    AxumPath(id): AxumPath<String>,
) -> Response {
    match fetch_backup(&state.pool, &id).await {
        Ok(Some(b)) => Json(backup_json(&b)).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "BACKUP_NOT_FOUND", "no such backup"),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to load backup",
        ),
    }
}

/// GET /api/v1/backups/:id/download — stream the tar.gz, `404` when the file
/// is missing from disk.
pub async fn download_backup(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let request_id = audit::request_id();
    let backup = match fetch_backup(&state.pool, &id).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return error(StatusCode::NOT_FOUND, "BACKUP_NOT_FOUND", "no such backup");
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load backup",
            );
        }
    };
    if backup.storage_path.is_empty() {
        return error(
            StatusCode::NOT_FOUND,
            "BACKUP_NOT_FOUND",
            "backup file is not available",
        );
    }
    let bytes = match tokio::fs::read(&backup.storage_path).await {
        Ok(b) => b,
        Err(_) => {
            return error(
                StatusCode::NOT_FOUND,
                "BACKUP_NOT_FOUND",
                "backup file is missing from disk",
            );
        }
    };
    audit::record(
        &state.pool,
        Some(&user.id),
        "backup.download",
        "backup",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "size_bytes": bytes.len() }),
    )
    .await;
    (
        [
            ("content-type", "application/gzip".to_string()),
            (
                "content-disposition",
                format!("attachment; filename=\"backup-{id}.tar.gz\""),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// DELETE /api/v1/backups/:id — `204`. Floor: protected rows and the last
/// completed (verified) backup of a server are never deleted.
pub async fn delete_backup(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let request_id = audit::request_id();
    let backup = match fetch_backup(&state.pool, &id).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return error(StatusCode::NOT_FOUND, "BACKUP_NOT_FOUND", "no such backup");
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load backup",
            );
        }
    };
    if backup.protected != 0 {
        return backup_error(BackupError::Protected);
    }
    if backup.state == "completed" {
        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM backups WHERE server_id = ? AND state = 'completed'",
        )
        .bind(&backup.server_id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
        if completed <= 1 {
            return error(
                StatusCode::CONFLICT,
                "BACKUP_PROTECTED",
                "refusing to delete the last verified backup",
            );
        }
    }
    if !backup.storage_path.is_empty() {
        let _ = tokio::fs::remove_file(&backup.storage_path).await;
    }
    let _ = sqlx::query("DELETE FROM backups WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await;
    audit::record(
        &state.pool,
        Some(&user.id),
        "backup.delete",
        "backup",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "server_id": backup.server_id }),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct RestoreRequest {
    #[serde(default = "default_true")]
    pub pre_restore_backup: bool,
}

fn default_true() -> bool {
    true
}

/// POST /api/v1/backups/:id/restore — enqueue a restore, `202`. Requires the
/// server to be stopped (`desired_state`); the check happens here so a
/// running server never triggers a pre-restore backup.
pub async fn restore_backup(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<RestoreRequest>,
) -> Response {
    let request_id = audit::request_id();
    let backup = match fetch_backup(&state.pool, &id).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            return error(StatusCode::NOT_FOUND, "BACKUP_NOT_FOUND", "no such backup");
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load backup",
            );
        }
    };
    let server = match fetch_server(&state.pool, &backup.server_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load server",
            );
        }
    };
    if server.desired_state == "running" {
        return error(
            StatusCode::CONFLICT,
            "SERVER_RUNNING",
            "stop the server before restoring a backup",
        );
    }
    if !try_lock(&state.op_locks, &server.id).await {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another operation is in flight for this server",
        );
    }
    let outcome = enqueue_restore(
        &state.pool,
        state.adapter.as_deref(),
        &state.backup_dir,
        &id,
        Some(&user.id),
        body.pre_restore_backup,
    )
    .await;
    unlock(&state.op_locks, &server.id).await;
    match outcome {
        Ok(task_id) => {
            audit::record(
                &state.pool,
                Some(&user.id),
                "restore.start",
                "backup",
                &id,
                "accepted",
                &request_id,
                serde_json::json!({
                    "server_id": server.id,
                    "task_id": task_id,
                    "pre_restore_backup": body.pre_restore_backup,
                }),
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
                "restore.start",
                "backup",
                &id,
                "failed",
                &request_id,
                serde_json::json!({ "error_code": e.code() }),
            )
            .await;
            backup_error(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (controller wiring mirror: the production router gains these routes
// when the controller lands; until then tests define the identical gates)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;
    use serde_json::{json, Value};

    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    // Tolerate poisoning: a failed test must not cascade-fail the rest.
    async fn test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().await
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();
        for file in [
            include_str!("../../../migrations/0001_init.sql"),
            include_str!("../../../migrations/0002_sessions_audit.sql"),
            include_str!("../../../migrations/0005_backups.sql"),
        ] {
            // Strip `--` comment lines first: headers may contain `;`
            // (e.g. 0002's "immutable; new tables only"), which would split
            // mid-comment into bogus chunks.
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
    fn test_router(state: AppState) -> axum::Router {
        use crate::auth::Permission;
        use axum::{
            middleware,
            routing::{delete, get, post},
        };

        let pool = state.pool.clone();
        let public = axum::Router::new()
            .route(
                "/api/v1/auth/setup-status",
                get(crate::routes_auth::setup_status),
            )
            .route("/api/v1/auth/setup", post(crate::routes_auth::setup))
            .route("/api/v1/auth/login", post(crate::routes_auth::login));
        let protected = axum::Router::new()
            .route(
                "/api/v1/servers",
                post(crate::routes_servers::create_server).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::ServerCreate),
                )),
            )
            .route(
                "/api/v1/tasks/:id",
                get(crate::routes_admin::get_task).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::ServerRead),
                )),
            )
            .route(
                "/api/v1/servers/:id/backups",
                post(create_backup).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::ServerBackupCreate),
                )),
            )
            .route(
                "/api/v1/servers/:id/backups",
                get(list_backups).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::ServerRead),
                )),
            )
            .route(
                "/api/v1/backups/:id",
                get(get_backup).route_layer(middleware::from_fn(crate::auth::require_permission(
                    Permission::ServerRead,
                ))),
            )
            .route(
                "/api/v1/backups/:id/download",
                get(download_backup).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::ServerRead),
                )),
            )
            .route(
                "/api/v1/backups/:id",
                delete(delete_backup).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::ServerBackupCreate),
                )),
            )
            .route(
                "/api/v1/backups/:id/restore",
                post(restore_backup).route_layer(middleware::from_fn(
                    crate::auth::require_permission(Permission::ServerBackupRestore),
                )),
            )
            .route_layer(middleware::from_fn_with_state(
                pool,
                crate::auth::auth_middleware,
            ));
        public.merge(protected).with_state(state)
    }

    struct Fixture {
        server: TestServer,
        pool: SqlitePool,
        token: String,
        server_root: PathBuf,
        backup_dir: PathBuf,
        state: AppState,
    }

    async fn fixture() -> Fixture {
        let pool = test_pool().await;
        let server_root =
            std::env::temp_dir().join(format!("pm-bak-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&server_root).unwrap();
        let backup_dir = std::env::temp_dir().join(format!("pm-bak-dir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&backup_dir).unwrap();
        // Single source of truth: the configured dir travels in state, never
        // via the `PUMPKIN_MANAGER_BACKUP_DIR` env heuristic (deleted with I1).
        let mut state = AppState::for_tests(pool.clone(), server_root.clone());
        state.backup_dir = backup_dir.clone();
        let server = TestServer::new(test_router(state.clone())).unwrap();
        let res = server
            .post("/api/v1/auth/setup")
            .json(&json!({"username": "owner", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200, "setup: {}", res.text());
        let token = res.json::<Value>()["token"].as_str().unwrap().to_string();
        Fixture {
            server,
            pool,
            token,
            server_root,
            backup_dir,
            state,
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.server_root);
            let _ = std::fs::remove_dir_all(&self.backup_dir);
        }
    }

    async fn make_server(fx: &Fixture, name: &str) -> String {
        let res = fx
            .server
            .post("/api/v1/servers")
            .authorization_bearer(&fx.token)
            .json(&json!({"name": name, "game_port": 25565,
                          "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        res.json::<Value>()["server"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn data_dir(fx: &Fixture, server_id: &str) -> PathBuf {
        let row: (String,) = sqlx::query_as("SELECT data_path FROM servers WHERE id = ?")
            .bind(server_id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
        let dir = PathBuf::from(row.0);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn poll_terminal(fx: &Fixture, task_id: &str) -> Value {
        for _ in 0..200 {
            let res = fx
                .server
                .get(&format!("/api/v1/tasks/{task_id}"))
                .authorization_bearer(&fx.token)
                .await;
            assert_eq!(res.status_code(), 200, "task poll: {}", res.text());
            let body = res.json::<Value>();
            let state = body["state"].as_str().unwrap_or("").to_string();
            if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("task {task_id} did not reach a terminal state in time");
    }

    async fn backup_and_wait(fx: &Fixture, server_id: &str, consistency: &str) -> (String, String) {
        let res = fx
            .server
            .post(&format!("/api/v1/servers/{server_id}/backups"))
            .authorization_bearer(&fx.token)
            .json(&json!({"mode": "full", "consistency": consistency}))
            .await;
        assert_eq!(res.status_code(), 202, "body: {}", res.text());
        let body = res.json::<Value>();
        let task_id = body["task_id"].as_str().unwrap().to_string();
        let backup_id = body["backup"]["id"].as_str().unwrap().to_string();
        let task = poll_terminal(fx, &task_id).await;
        assert_eq!(
            task["state"],
            json!("succeeded"),
            "backup task failed: {}",
            task
        );
        (task_id, backup_id)
    }

    fn list_archive(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        use std::io::Read as _;
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);
        let mut out = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            out.push((name, content));
        }
        out
    }

    async fn backup_count(fx: &Fixture, server_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM backups WHERE server_id = ?")
            .bind(server_id)
            .fetch_one(&fx.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn manual_backup_completes_verified() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak1").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::create_dir_all(dir.join("world")).unwrap();
        std::fs::write(dir.join("world/level.dat"), b"level-bytes-v1").unwrap();
        std::fs::write(dir.join("server.properties"), b"motd=hi").unwrap();

        let (_task_id, backup_id) = backup_and_wait(&fx, &id, "live").await;

        // Row is completed with a real checksum + size.
        let res = fx
            .server
            .get(&format!("/api/v1/backups/{backup_id}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["state"], json!("completed"));
        assert!(body["size_bytes"].as_i64().unwrap() > 0);
        let checksum = body["checksum"].as_str().unwrap().to_string();
        assert_eq!(checksum.len(), 64);
        let storage = body["storage_path"].as_str().unwrap().to_string();

        // Checksum matches the file on disk, and the archive re-lists.
        let on_disk = std::fs::read(&storage).unwrap();
        assert_eq!(super::sha256_file(Path::new(&storage)).unwrap(), checksum);
        let entries = list_archive(&on_disk);
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.iter().any(|n| n.ends_with("world/level.dat")),
            "{names:?}"
        );
        let level = entries
            .iter()
            .find(|(n, _)| n.ends_with("world/level.dat"))
            .unwrap();
        assert_eq!(level.1, b"level-bytes-v1");

        // Download endpoint streams the same bytes.
        let dl = fx
            .server
            .get(&format!("/api/v1/backups/{backup_id}/download"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(dl.status_code(), 200, "body: {}", dl.text());
        assert_eq!(dl.as_bytes().as_ref(), on_disk.as_slice());

        // List shows the backup.
        let list = fx
            .server
            .get(&format!("/api/v1/servers/{id}/backups"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(list.status_code(), 200);
        assert_eq!(list.json::<Value>()["total"], json!(1));
    }

    #[tokio::test]
    async fn unsupported_mode_rejected() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak2").await;
        let res = fx
            .server
            .post(&format!("/api/v1/servers/{id}/backups"))
            .authorization_bearer(&fx.token)
            .json(&json!({"mode": "world", "consistency": "live"}))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
    }

    #[tokio::test]
    async fn restore_requires_stopped() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak3").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("a.txt"), b"v1").unwrap();
        let (_, backup_id) = backup_and_wait(&fx, &id, "live").await;

        sqlx::query("UPDATE servers SET desired_state = 'running' WHERE id = ?")
            .bind(&id)
            .execute(&fx.pool)
            .await
            .unwrap();
        let before = backup_count(&fx, &id).await;
        let res = fx
            .server
            .post(&format!("/api/v1/backups/{backup_id}/restore"))
            .authorization_bearer(&fx.token)
            .json(&json!({}))
            .await;
        assert_eq!(res.status_code(), 409, "body: {}", res.text());
        // No pre-restore backup was created.
        assert_eq!(backup_count(&fx, &id).await, before);
    }

    #[tokio::test]
    async fn restore_round_trip() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak4").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("data.txt"), b"original-bytes").unwrap();
        let (_, backup_id) = backup_and_wait(&fx, &id, "live").await;

        // Diverge live data, then restore with the server stopped.
        std::fs::write(dir.join("data.txt"), b"diverged-bytes").unwrap();
        std::fs::write(dir.join("extra.txt"), b"should-vanish").unwrap();
        let res = fx
            .server
            .post(&format!("/api/v1/backups/{backup_id}/restore"))
            .authorization_bearer(&fx.token)
            .json(&json!({}))
            .await;
        assert_eq!(res.status_code(), 202, "body: {}", res.text());
        let task_id = res.json::<Value>()["task_id"].as_str().unwrap().to_string();
        let task = poll_terminal(&fx, &task_id).await;
        assert_eq!(task["state"], json!("succeeded"), "restore failed: {task}");

        assert_eq!(
            std::fs::read(dir.join("data.txt")).unwrap(),
            b"original-bytes"
        );
        assert!(!dir.join("extra.txt").exists());

        // A protected pre-restore backup row exists.
        let pre: Vec<(String,)> = sqlx::query_as(
            "SELECT id FROM backups WHERE server_id = ? AND protected = 1 AND id != ?",
        )
        .bind(&id)
        .bind(&backup_id)
        .fetch_all(&fx.pool)
        .await
        .unwrap();
        assert_eq!(pre.len(), 1, "expected one protected pre-restore backup");
        let pre_row = fetch_backup(&fx.pool, &pre[0].0).await.unwrap().unwrap();
        assert_eq!(pre_row.state, "completed");
    }

    #[tokio::test]
    async fn corrupt_restore_rejected() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak5").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("keep.txt"), b"precious").unwrap();
        let (_, backup_id) = backup_and_wait(&fx, &id, "live").await;

        // Tamper with the archive on disk (checksum now mismatches).
        let storage: (String,) = sqlx::query_as("SELECT storage_path FROM backups WHERE id = ?")
            .bind(&backup_id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
        let mut bytes = std::fs::read(&storage.0).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&storage.0, &bytes).unwrap();

        let res = fx
            .server
            .post(&format!("/api/v1/backups/{backup_id}/restore"))
            .authorization_bearer(&fx.token)
            .json(&json!({}))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
        assert_eq!(res.json::<Value>()["error_code"], json!("BACKUP_CORRUPT"));
        // Live data untouched.
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"precious");
    }

    #[tokio::test]
    async fn foreign_backup_rejected() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak6").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("keep.txt"), b"precious").unwrap();
        let (_, backup_id) = backup_and_wait(&fx, &id, "live").await;

        // Repoint the row at a server that does not exist (FK enforcement
        // off for this write only: the dangling row is the fixture).
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&fx.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE backups SET server_id = 'does-not-exist' WHERE id = ?")
            .bind(&backup_id)
            .execute(&fx.pool)
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&fx.pool)
            .await
            .unwrap();
        let res = fx
            .server
            .post(&format!("/api/v1/backups/{backup_id}/restore"))
            .authorization_bearer(&fx.token)
            .json(&json!({}))
            .await;
        assert!(
            [404, 422].contains(&res.status_code().as_u16()),
            "body: {}",
            res.text()
        );
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"precious");
    }

    #[tokio::test]
    async fn retention_floor() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak7").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("a.txt"), b"v1").unwrap();
        let (_, b1) = backup_and_wait(&fx, &id, "live").await;

        // Last verified backup is never deleted.
        let del = fx
            .server
            .delete(&format!("/api/v1/backups/{b1}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(del.status_code(), 409, "body: {}", del.text());
        assert_eq!(del.json::<Value>()["error_code"], json!("BACKUP_PROTECTED"));

        // With a second verified backup, the older unprotected one goes.
        let (_, b2) = backup_and_wait(&fx, &id, "live").await;
        let del = fx
            .server
            .delete(&format!("/api/v1/backups/{b1}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(del.status_code(), 204, "body: {}", del.text());
        let gone = fx
            .server
            .get(&format!("/api/v1/backups/{b1}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(gone.status_code(), 404);

        // The remaining one is the last verified backup again.
        let del = fx
            .server
            .delete(&format!("/api/v1/backups/{b2}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(del.status_code(), 409);

        // Protected rows are never deleted.
        sqlx::query("UPDATE backups SET protected = 1 WHERE id = ?")
            .bind(&b2)
            .execute(&fx.pool)
            .await
            .unwrap();
        let del = fx
            .server
            .delete(&format!("/api/v1/backups/{b2}"))
            .authorization_bearer(&fx.token)
            .await;
        assert_eq!(del.status_code(), 409, "body: {}", del.text());
        assert_eq!(del.json::<Value>()["error_code"], json!("BACKUP_PROTECTED"));
    }

    #[tokio::test]
    async fn pre_restore_failure_aborts() {
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak8").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("keep.txt"), b"precious").unwrap();
        let (_, backup_id) = backup_and_wait(&fx, &id, "live").await;

        // Break backup storage deterministically: a router whose configured
        // `backup_dir` is a regular file, so even the storage probe fails
        // (works as any user, root or not).
        let blocker = fx.backup_dir.join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let mut bad = fx.state.clone();
        bad.backup_dir = blocker;
        let bad_server = TestServer::new(test_router(bad)).unwrap();

        let res = bad_server
            .post(&format!("/api/v1/backups/{backup_id}/restore"))
            .authorization_bearer(&fx.token)
            .json(&json!({}))
            .await;
        assert!(
            [409, 502].contains(&res.status_code().as_u16()),
            "body: {}",
            res.text()
        );
        // Live data untouched, and no pre-restore row was left behind.
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"precious");
        let pre: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM backups WHERE server_id = ? AND protected = 1",
        )
        .bind(&id)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        assert_eq!(pre, 0);
    }

    async fn seed_completed(
        fx: &Fixture,
        server_id: &str,
        bid: &str,
        created_at: &str,
        protected: bool,
    ) {
        let path = fx.backup_dir.join(format!("{bid}.tar.gz"));
        std::fs::write(&path, b"seed-bytes").unwrap();
        sqlx::query(
            "INSERT INTO backups
                (id, server_id, type, state, storage_path, size_bytes,
                 checksum_algorithm, checksum, consistency_method,
                 created_at, completed_at, protected)
             VALUES (?, ?, 'full', 'completed', ?, 10, 'sha256', 'abc', 'live', ?, ?, ?)",
        )
        .bind(bid)
        .bind(server_id)
        .bind(path.to_string_lossy().into_owned())
        .bind(created_at)
        .bind(created_at)
        .bind(i64::from(protected))
        .execute(&fx.pool)
        .await
        .unwrap();
    }

    async fn backup_ids(fx: &Fixture, server_id: &str) -> Vec<String> {
        sqlx::query_as::<_, (String,)>("SELECT id FROM backups WHERE server_id = ?")
            .bind(server_id)
            .fetch_all(&fx.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|(id,)| id)
            .collect()
    }

    #[tokio::test]
    async fn prune_uses_canonical_retention_decisions() {
        // Pins FIX 1: the worker prune goes through Task 1's
        // `select_for_deletion`, whose protected rows consume keep slots.
        // Six unprotected seeds + one newest protected seed, then one real
        // backup: canonical ranks [new, protected, c6..c1] and deletes only
        // the oldest unprotected seed. The old inline copy kept 7
        // unprotected and would delete nothing.
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak-prune").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("a.txt"), b"v1").unwrap();

        for (i, bid) in ["c1", "c2", "c3", "c4", "c5", "c6"].iter().enumerate() {
            seed_completed(
                &fx,
                &id,
                bid,
                &format!("2026-01-01T00:00:0{}Z", i + 1),
                false,
            )
            .await;
        }
        seed_completed(&fx, &id, "p1", "2026-01-01T00:00:07Z", true).await;
        // keep=7 with no age expiry: only the keep-slot rule applies, so the
        // protected seed consuming a slot is what dooms the oldest seed.
        insert_policy(&fx, &id, 7, 0).await;

        let (_, fresh) = backup_and_wait(&fx, &id, "live").await;

        let mut remaining = backup_ids(&fx, &id).await;
        remaining.sort();
        let mut expected = vec!["c2", "c3", "c4", "c5", "c6", "p1", fresh.as_str()];
        expected.sort_unstable();
        assert_eq!(remaining, expected, "canonical prune deleted wrong rows");

        // The executor removed the pruned file and kept the protected one.
        assert!(!fx.backup_dir.join("c1.tar.gz").exists());
        assert!(fx.backup_dir.join("p1.tar.gz").exists());
    }

    async fn insert_policy(fx: &Fixture, server_id: &str, keep: i64, max_age: i64) {
        sqlx::query(
            "INSERT INTO backup_policies
                (id, server_id, name, schedule, timezone,
                 retention_count, max_age_days, enabled)
             VALUES (?, ?, 'test', '0 2 * * *', 'UTC', ?, ?, 1)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(server_id)
        .bind(keep)
        .bind(max_age)
        .execute(&fx.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn worker_prune_honors_server_retention_policy() {
        // FIX round 2: the post-backup prune uses the server's retention
        // rule (most recently updated enabled policy, else 7/30). keep=1
        // with two pre-existing completed backups + one protected row:
        // after one successful backup exactly 1 unprotected completed
        // backup remains and the protected row is never pruned.
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak-policy").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("a.txt"), b"v1").unwrap();
        insert_policy(&fx, &id, 1, 0).await;
        seed_completed(&fx, &id, "k1", "2026-09-01T00:00:01Z", false).await;
        seed_completed(&fx, &id, "k2", "2026-09-01T00:00:02Z", false).await;
        seed_completed(&fx, &id, "kp", "2026-09-01T00:00:03Z", true).await;

        let (_, fresh) = backup_and_wait(&fx, &id, "live").await;

        let remaining = backup_ids(&fx, &id).await;
        assert_eq!(remaining.len(), 2, "wrong survivors: {remaining:?}");
        assert!(remaining.contains(&fresh));
        assert!(remaining.iter().any(|b| b == "kp"));
        assert!(!fx.backup_dir.join("k1.tar.gz").exists());
        assert!(!fx.backup_dir.join("k2.tar.gz").exists());
        assert!(fx.backup_dir.join("kp.tar.gz").exists());
    }

    #[tokio::test]
    async fn worker_prune_defaults_without_policy() {
        // No enabled policy → table defaults (keep 7, 30 days): a backup
        // older than 30 days is pruned by age even though the keep count
        // is not exceeded.
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak-policy-default").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("a.txt"), b"v1").unwrap();
        seed_completed(&fx, &id, "old", "2026-01-01T00:00:01Z", false).await;

        let (_, fresh) = backup_and_wait(&fx, &id, "live").await;

        let remaining = backup_ids(&fx, &id).await;
        assert_eq!(remaining, vec![fresh], "wrong survivors: {remaining:?}");
        assert!(!fx.backup_dir.join("old.tar.gz").exists());
    }

    #[tokio::test]
    async fn shared_op_lock_guards_backup_enqueue() {
        // Pins FIX 2: the backup handler claims the shared
        // `AppState::op_locks`, so a lifecycle-style holder makes backup
        // creation fail fast with `SERVER_BUSY` (and nothing is enqueued).
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak-lock1").await;

        fx.state.op_locks.lock().await.insert(id.clone());
        let busy = fx
            .server
            .post(&format!("/api/v1/servers/{id}/backups"))
            .authorization_bearer(&fx.token)
            .json(&json!({"mode": "full", "consistency": "live"}))
            .await;
        assert_eq!(busy.status_code(), 409, "body: {}", busy.text());
        assert_eq!(busy.json::<Value>()["error_code"], json!("SERVER_BUSY"));
        assert_eq!(backup_count(&fx, &id).await, 0);
        fx.state.op_locks.lock().await.remove(&id);

        let res = fx
            .server
            .post(&format!("/api/v1/servers/{id}/backups"))
            .authorization_bearer(&fx.token)
            .json(&json!({"mode": "full", "consistency": "live"}))
            .await;
        assert_eq!(res.status_code(), 202, "body: {}", res.text());
        let task_id = res.json::<Value>()["task_id"].as_str().unwrap().to_string();
        let task = poll_terminal(&fx, &task_id).await;
        assert_eq!(task["state"], json!("succeeded"), "backup failed: {task}");
    }

    #[tokio::test]
    async fn shared_op_lock_guards_restore_enqueue() {
        // Same shared-lock gate on the restore path.
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak-lock2").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("a.txt"), b"v1").unwrap();
        let (_, backup_id) = backup_and_wait(&fx, &id, "live").await;

        fx.state.op_locks.lock().await.insert(id.clone());
        let busy = fx
            .server
            .post(&format!("/api/v1/backups/{backup_id}/restore"))
            .authorization_bearer(&fx.token)
            .json(&json!({}))
            .await;
        assert_eq!(busy.status_code(), 409, "body: {}", busy.text());
        assert_eq!(busy.json::<Value>()["error_code"], json!("SERVER_BUSY"));
        fx.state.op_locks.lock().await.remove(&id);

        let res = fx
            .server
            .post(&format!("/api/v1/backups/{backup_id}/restore"))
            .authorization_bearer(&fx.token)
            .json(&json!({}))
            .await;
        assert_eq!(res.status_code(), 202, "body: {}", res.text());
        let task_id = res.json::<Value>()["task_id"].as_str().unwrap().to_string();
        let task = poll_terminal(&fx, &task_id).await;
        assert_eq!(task["state"], json!("succeeded"), "restore failed: {task}");
    }

    #[tokio::test]
    async fn backup_storage_honors_configured_backup_dir() {
        // I1: `AppState::backup_dir` is the single source of truth. With no
        // env override present, the archive must land in the configured dir,
        // not the `<server_root>/../backups` sibling heuristic.
        let _guard = test_guard().await;
        std::env::remove_var("PUMPKIN_MANAGER_BACKUP_DIR");
        let fx = fixture().await;
        let id = make_server(&fx, "bak-dir-truth").await;
        let dir = data_dir(&fx, &id).await;
        std::fs::write(dir.join("a.txt"), b"v1").unwrap();

        let (_, backup_id) = backup_and_wait(&fx, &id, "live").await;

        let storage: (String,) = sqlx::query_as("SELECT storage_path FROM backups WHERE id = ?")
            .bind(&backup_id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
        assert_eq!(
            storage.0,
            fx.backup_dir
                .join(format!("{backup_id}.tar.gz"))
                .to_string_lossy()
                .into_owned(),
            "archive must live in the configured backup dir"
        );
        assert!(Path::new(&storage.0).is_file());
    }

    #[tokio::test]
    async fn startup_cleanup_removes_stale_staging() {
        let _guard = test_guard().await;
        let pool = test_pool().await;
        let dir = std::env::temp_dir().join(format!("pm-bak-clean-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".tmp-stale-1")).unwrap();
        std::fs::write(dir.join(".tmp-stale-1/chunk"), b"x").unwrap();
        std::fs::write(dir.join("real-backup.tar.gz"), b"keep").unwrap();
        startup_cleanup(&pool, &dir).await;
        assert!(!dir.join(".tmp-stale-1").exists());
        assert!(dir.join("real-backup.tar.gz").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn startup_cleanup_cleans_server_staging_and_recovery() {
        // I4: crashed restores leave `.tmp-restore-*` staging and
        // `.recovery-*` dirs beside the server data dir. Boot cleanup must
        // remove them (older than boot) while leaving live data alone.
        let _guard = test_guard().await;
        let fx = fixture().await;
        let id = make_server(&fx, "bak-cleanup").await;
        let dir = data_dir(&fx, &id).await;
        let parent = dir.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(parent.join(".tmp-restore-stale")).unwrap();
        std::fs::write(parent.join(".tmp-restore-stale/chunk"), b"x").unwrap();
        std::fs::create_dir_all(parent.join(".recovery-12345")).unwrap();
        std::fs::write(parent.join(".recovery-12345/saved"), b"y").unwrap();
        std::fs::write(dir.join("live.txt"), b"live").unwrap();

        startup_cleanup(&fx.pool, &fx.backup_dir).await;

        assert!(!parent.join(".tmp-restore-stale").exists());
        assert!(!parent.join(".recovery-12345").exists());
        assert_eq!(std::fs::read(dir.join("live.txt")).unwrap(), b"live");
    }
}
