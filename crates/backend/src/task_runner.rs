//! Phase 2 workstream B — bounded background task worker.
//!
//! Long-running server operations (`start`, `stop`, `restart`, `remove`, …)
//! are recorded as rows in the existing `tasks` table and executed by a
//! small pool of background workers:
//!
//! * [`enqueue`] persists a `queued` task and returns its [`TaskRecord`].
//! * [`spawn_runner`] starts a supervisor holding an `mpsc` queue plus a
//!   [`tokio::sync::Semaphore`] that caps concurrent Docker operations
//!   (default [`DEFAULT_CONCURRENCY`]).
//! * [`requeue_interrupted`] marks tasks left `queued`/`running` by a
//!   previous process as `failed` with `TASK_INTERRUPTED`. Call it once on
//!   boot before [`spawn_runner`].
//! * [`mark_running`] / [`mark_succeeded`] / [`mark_failed`] are the only
//!   state transitions workers use.
//!
//! No HTTP code lives here; HTTP handlers (another workstream) call
//! [`enqueue`] and submit the id through [`RunnerHandle::submit`].
//!
//! Uses only the existing `tasks` (and `servers`, for container lookup)
//! tables — no new SQL objects required.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, Semaphore};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::runtime::{DockerAdapter, RuntimeError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default cap on concurrent Docker operations.
pub const DEFAULT_CONCURRENCY: usize = 4;
/// Bounded in-memory queue depth for pending task ids.
pub const QUEUE_DEPTH: usize = 128;
/// Stop grace period used by the `stop` task.
pub const TASK_STOP_TIMEOUT_SECS: u32 = 35;

// ---------------------------------------------------------------------------
// Task model
// ---------------------------------------------------------------------------

/// Lifecycle states, mirroring the `tasks.state` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    /// Waiting for a worker.
    Queued,
    /// Currently executing.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Finished with an error (see `error_code`).
    Failed,
    /// Cancellation was requested; worker should abort when safe.
    CancelRequested,
    /// Cancelled before/without doing work.
    Cancelled,
}

impl TaskState {
    /// DB wire value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::str::FromStr for TaskState {
    type Err = ();

    /// Parse a DB wire value.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancel_requested" => Ok(Self::CancelRequested),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(()),
        }
    }
}

/// One row of the `tasks` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// Task id (UUID string).
    pub id: String,
    /// Operation type (`start`, `stop`, `restart`, `remove`, …).
    pub task_type: String,
    /// Owning server id, if any.
    pub server_id: Option<String>,
    /// Requesting user id, if any.
    pub requested_by: Option<String>,
    /// Current state.
    pub state: TaskState,
    /// 0–100 progress hint.
    pub progress_percent: i32,
    /// Human-readable status message.
    pub message: String,
    /// Machine-facing error code (set on failure).
    pub error_code: Option<String>,
    /// Human-readable error summary (set on failure).
    pub error_summary: Option<String>,
}

/// Task worker failures.
#[derive(Debug, Error)]
pub enum TaskError {
    /// Task type string empty.
    #[error("task type must not be empty")]
    EmptyType,
    /// Referenced task row does not exist.
    #[error("task not found: {0}")]
    NotFound(String),
    /// Referenced server row does not exist.
    #[error("server not found: {0}")]
    ServerNotFound(String),
    /// Server row has no container to operate on.
    #[error("server {0} has no container id")]
    MissingContainer(String),
    /// No worker implements this task type.
    #[error("unknown task type: {0}")]
    UnknownTaskType(String),
    /// Underlying SQLite failure.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// Queue is full / receiver gone.
    #[error("task queue error: {0}")]
    Queue(String),
}

// ---------------------------------------------------------------------------
// DB helpers
// ---------------------------------------------------------------------------

fn row_to_record(r: &sqlx::sqlite::SqliteRow) -> Result<TaskRecord, TaskError> {
    use sqlx::Row;
    let state: String = r.try_get("state")?;
    let state: TaskState = state
        .parse()
        .map_err(|()| TaskError::Database(sqlx::Error::Decode("unknown task state".into())))?;
    Ok(TaskRecord {
        id: r.try_get("id")?,
        task_type: r.try_get("type")?,
        server_id: r.try_get("server_id")?,
        requested_by: r.try_get("requested_by")?,
        state,
        progress_percent: r.try_get("progress_percent")?,
        message: r.try_get("message")?,
        error_code: r.try_get("error_code")?,
        error_summary: r.try_get("error_summary")?,
    })
}

/// Fetch a task by id.
pub async fn get(pool: &sqlx::SqlitePool, task_id: &str) -> Result<Option<TaskRecord>, TaskError> {
    let row = sqlx::query(
        "SELECT id, type, server_id, requested_by, state, progress_percent, message, error_code, error_summary FROM tasks WHERE id = ?",
    )
    .bind(task_id)
    .fetch_optional(pool)
    .await?;
    match row {
        None => Ok(None),
        Some(ref r) => Ok(Some(row_to_record(r)?)),
    }
}

/// Persist a new `queued` task and return it.
pub async fn enqueue(
    pool: &sqlx::SqlitePool,
    task_type: &str,
    server_id: Option<&str>,
    requested_by: Option<&str>,
) -> Result<TaskRecord, TaskError> {
    if task_type.trim().is_empty() {
        return Err(TaskError::EmptyType);
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO tasks (id, type, server_id, requested_by, state) VALUES (?, ?, ?, ?, 'queued')",
    )
    .bind(&id)
    .bind(task_type)
    .bind(server_id)
    .bind(requested_by)
    .execute(pool)
    .await?;
    info!(task_id = %id, task_type, "task enqueued");
    get(pool, &id).await?.ok_or_else(|| TaskError::NotFound(id))
}

/// Mark a task `running` (sets `started_at`, clears stale errors).
pub async fn mark_running(
    pool: &sqlx::SqlitePool,
    task_id: &str,
    message: &str,
) -> Result<(), TaskError> {
    sqlx::query(
        "UPDATE tasks SET state = 'running', message = ?, error_code = NULL, error_summary = NULL, started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(message)
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a task `succeeded` (progress 100, sets `finished_at`).
pub async fn mark_succeeded(
    pool: &sqlx::SqlitePool,
    task_id: &str,
    message: &str,
) -> Result<(), TaskError> {
    sqlx::query(
        "UPDATE tasks SET state = 'succeeded', progress_percent = 100, message = ?, finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(message)
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a task `failed` with a machine-facing code and summary.
pub async fn mark_failed(
    pool: &sqlx::SqlitePool,
    task_id: &str,
    error_code: &str,
    error_summary: &str,
) -> Result<(), TaskError> {
    sqlx::query(
        "UPDATE tasks SET state = 'failed', error_code = ?, error_summary = ?, finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(error_code)
    .bind(error_summary)
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a task `cancelled` (sets `finished_at`). Only valid pre-dispatch.
pub async fn mark_cancelled(pool: &sqlx::SqlitePool, task_id: &str) -> Result<(), TaskError> {
    sqlx::query(
        "UPDATE tasks SET state = 'cancelled', finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
    )
    .bind(task_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Crash recovery: fail every task left `queued` or `running` by a previous
/// process with `TASK_INTERRUPTED`. Call once on boot, before
/// [`spawn_runner`]. Returns the number of rows failed.
pub async fn requeue_interrupted(pool: &sqlx::SqlitePool) -> Result<u64, TaskError> {
    let res = sqlx::query(
        "UPDATE tasks SET state = 'failed', error_code = 'TASK_INTERRUPTED', error_summary = 'Manager restarted while the task was in flight', finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE state IN ('queued', 'running')",
    )
    .execute(pool)
    .await?;
    let n = res.rows_affected();
    if n > 0 {
        warn!(count = n, "marked interrupted tasks as failed");
    }
    Ok(n)
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// Record a task failure, truncating the summary to a sane length.
async fn fail_task(
    pool: &sqlx::SqlitePool,
    task_id: &str,
    code: &str,
    summary: &str,
) -> Result<(), TaskError> {
    let summary: String = summary.chars().take(500).collect();
    mark_failed(pool, task_id, code, &summary).await
}

/// Look up the Docker container id for a server row.
async fn container_for_server(
    pool: &sqlx::SqlitePool,
    server_id: &str,
) -> Result<String, TaskError> {
    use sqlx::Row;
    let row = sqlx::query("SELECT container_id FROM servers WHERE id = ? AND deleted_at IS NULL")
        .bind(server_id)
        .fetch_optional(pool)
        .await?;
    match row {
        None => Err(TaskError::ServerNotFound(server_id.to_string())),
        Some(r) => {
            let cid: String = r.try_get("container_id")?;
            if cid.is_empty() {
                Err(TaskError::MissingContainer(server_id.to_string()))
            } else {
                Ok(cid)
            }
        }
    }
}

/// Execute a single task to a terminal state.
///
/// Handles runtime ops (`start`/`stop`/`restart`/`remove`) and Phase 7 image
/// ops (`update`/`rollback`, dispatched to `update_service` executors);
/// anything else is failed with `UNKNOWN_TASK_TYPE` so new task kinds fail
/// loudly instead of silently no-opping. (The backup workstream extends this
/// match.)
///
/// Generic over [`RuntimeAdapter`] so cancellation paths are unit-testable
/// without a daemon.
pub async fn dispatch_task<A: super::runtime::RuntimeAdapter>(
    pool: &sqlx::SqlitePool,
    adapter: &A,
    task_id: &str,
) -> Result<(), TaskError> {
    let Some(task) = get(pool, task_id).await? else {
        warn!(task_id, "dispatch for unknown task");
        return Err(TaskError::NotFound(task_id.to_string()));
    };
    // Cooperative cancellation is decided before any runtime call: a task
    // cancelled while queued never touches Docker.
    if matches!(task.state, TaskState::CancelRequested) {
        info!(task_id, "task cancelled before dispatch");
        mark_cancelled(pool, task_id).await?;
        return Ok(());
    }
    // Only claim queued (or previously interrupted) work; never stomp a
    // terminal state or another worker's running task.
    if !matches!(task.state, TaskState::Queued) {
        return Ok(());
    }

    match task.task_type.as_str() {
        "start" | "stop" | "restart" | "remove" => {
            let Some(server_id) = task.server_id.clone() else {
                fail_task(
                    pool,
                    task_id,
                    "INVALID_CONFIGURATION",
                    "task has no server_id",
                )
                .await?;
                return Ok(());
            };
            let container_id = match container_for_server(pool, &server_id).await {
                Ok(cid) => cid,
                Err(TaskError::ServerNotFound(_)) => {
                    fail_task(
                        pool,
                        task_id,
                        "SERVER_NOT_FOUND",
                        &format!("server {server_id} not found"),
                    )
                    .await?;
                    return Ok(());
                }
                Err(TaskError::MissingContainer(_)) => {
                    fail_task(
                        pool,
                        task_id,
                        "MISSING_CONTAINER",
                        &format!("server {server_id} has no container"),
                    )
                    .await?;
                    return Ok(());
                }
                Err(e) => return Err(e),
            };

            mark_running(pool, task_id, &format!("{} in progress", task.task_type)).await?;
            let outcome: Result<String, RuntimeError> = match task.task_type.as_str() {
                "start" => adapter
                    .start(&container_id)
                    .await
                    .map(|()| "server started".to_string()),
                "stop" => adapter
                    .stop(&container_id, TASK_STOP_TIMEOUT_SECS)
                    .await
                    .map(|()| "server stopped".to_string()),
                "restart" => adapter
                    .restart(&container_id)
                    .await
                    .map(|()| "server restarted".to_string()),
                // Data survives: servers use bind-mounts, never volumes.
                "remove" => adapter
                    .remove(&container_id)
                    .await
                    .map(|()| "container removed".to_string()),
                _ => unreachable!(),
            };
            match outcome {
                Ok(msg) => {
                    info!(task_id, task_type = %task.task_type, "task succeeded");
                    mark_succeeded(pool, task_id, &msg).await?;
                    // Eager observed-state sync: status reads stay truthful
                    // even before the next inspect pass.
                    let observed = match task.task_type.as_str() {
                        "start" | "restart" => Some("running"),
                        "stop" => Some("exited"),
                        "remove" => Some("missing"),
                        _ => None,
                    };
                    if let Some(observed) = observed {
                        let _ = sqlx::query(
                            "UPDATE servers SET observed_state = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?",
                        )
                        .bind(observed)
                        .bind(&server_id)
                        .execute(pool)
                        .await;
                    }
                }
                Err(e) => {
                    error!(task_id, error = %e, "task failed");
                    fail_task(pool, task_id, e.code(), &e.to_string()).await?;
                }
            }
        }
        "update" => {
            crate::update_service::execute_update(pool, adapter, task_id).await?;
        }
        "rollback" => {
            crate::update_service::execute_rollback(pool, adapter, task_id).await?;
        }
        other => {
            warn!(task_id, task_type = other, "no worker for task type");
            // Mark running first so the state trail shows the claim.
            mark_running(pool, task_id, "rejecting unknown task type").await?;
            fail_task(
                pool,
                task_id,
                "UNKNOWN_TASK_TYPE",
                &format!("no worker implements task type '{other}'"),
            )
            .await?;
        }
    }
    Ok(())
}

/// Handle to the background runner. Cloneable; submit task ids through it.
#[derive(Debug, Clone)]
pub struct RunnerHandle {
    tx: mpsc::Sender<String>,
}

impl RunnerHandle {
    /// Queue a task id for execution. Returns an error if the runner is
    /// gone or the queue is full (callers should surface a 503/retry).
    pub async fn submit(&self, task_id: &str) -> Result<(), TaskError> {
        self.tx
            .try_send(task_id.to_string())
            .map_err(|e| TaskError::Queue(e.to_string()))
    }
}

/// Start the background runner.
///
/// Spawns a supervisor holding the queue receiver; each received task id is
/// executed in its own task under `semaphore` (cap `concurrency`, default
/// [`DEFAULT_CONCURRENCY`] when 0). Returns a [`RunnerHandle`] for
/// submitting work.
pub fn spawn_runner(
    pool: sqlx::SqlitePool,
    adapter: Arc<DockerAdapter>,
    concurrency: usize,
) -> RunnerHandle {
    let concurrency = if concurrency == 0 {
        DEFAULT_CONCURRENCY
    } else {
        concurrency
    };
    let (tx, mut rx) = mpsc::channel::<String>(QUEUE_DEPTH);
    let semaphore = Arc::new(Semaphore::new(concurrency));

    tokio::spawn(async move {
        info!(concurrency, "task runner started");
        while let Some(task_id) = rx.recv().await {
            let pool = pool.clone();
            let adapter = adapter.clone();
            let semaphore = semaphore.clone();
            tokio::spawn(async move {
                let _permit = semaphore.acquire_owned().await;
                if let Err(e) = dispatch_task(&pool, adapter.as_ref(), &task_id).await {
                    error!(%task_id, error = %e, "dispatch failed");
                }
            });
        }
        warn!("task runner queue closed; supervisor exiting");
    });

    RunnerHandle { tx }
}

/// Convenience: [`enqueue`] a task and hand its id to the runner.
pub async fn enqueue_and_submit(
    pool: &sqlx::SqlitePool,
    runner: &RunnerHandle,
    task_type: &str,
    server_id: Option<&str>,
    requested_by: Option<&str>,
) -> Result<TaskRecord, TaskError> {
    let record = enqueue(pool, task_type, server_id, requested_by).await?;
    runner.submit(&record.id).await?;
    Ok(record)
}

// ---------------------------------------------------------------------------
// Unit tests (sqlite :memory:, single connection)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        sqlx::query(
            "CREATE TABLE servers (
                id TEXT PRIMARY KEY,
                container_id TEXT NOT NULL DEFAULT '',
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
        pool
    }

    #[test]
    fn task_state_round_trips() {
        for s in [
            TaskState::Queued,
            TaskState::Running,
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::CancelRequested,
            TaskState::Cancelled,
        ] {
            assert_eq!(s.as_str().parse(), Ok(s));
        }
        assert_eq!("bogus".parse::<TaskState>(), Err(()));
    }

    #[tokio::test]
    async fn enqueue_get_and_transitions() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO servers (id, container_id) VALUES ('srv-1', 'cid-1')")
            .execute(&pool)
            .await
            .expect("server row");
        let rec = enqueue(&pool, "start", Some("srv-1"), Some("user-1"))
            .await
            .expect("enqueue");
        assert_eq!(rec.state, TaskState::Queued);
        assert_eq!(rec.task_type, "start");

        mark_running(&pool, &rec.id, "going")
            .await
            .expect("running");
        let r = get(&pool, &rec.id).await.expect("get").expect("row");
        assert_eq!(r.state, TaskState::Running);

        mark_succeeded(&pool, &rec.id, "done").await.expect("done");
        let r = get(&pool, &rec.id).await.expect("get").expect("row");
        assert_eq!(r.state, TaskState::Succeeded);
        assert_eq!(r.progress_percent, 100);

        let rec2 = enqueue(&pool, "stop", None, None).await.expect("enqueue2");
        mark_failed(&pool, &rec2.id, "SOME_CODE", "boom")
            .await
            .expect("failed");
        let r2 = get(&pool, &rec2.id).await.expect("get").expect("row");
        assert_eq!(r2.state, TaskState::Failed);
        assert_eq!(r2.error_code.as_deref(), Some("SOME_CODE"));
    }

    #[tokio::test]
    async fn rejects_empty_type() {
        let pool = test_pool().await;
        assert!(matches!(
            enqueue(&pool, "  ", None, None).await,
            Err(TaskError::EmptyType)
        ));
    }

    #[tokio::test]
    async fn requeue_interrupted_fails_inflight_only() {
        let pool = test_pool().await;
        let q = enqueue(&pool, "start", None, None).await.expect("q");
        let r = enqueue(&pool, "stop", None, None).await.expect("r");
        mark_running(&pool, &r.id, "going").await.expect("run");
        let done = enqueue(&pool, "restart", None, None).await.expect("done");
        mark_succeeded(&pool, &done.id, "ok").await.expect("ok");

        let n = requeue_interrupted(&pool).await.expect("requeue");
        assert_eq!(n, 2);

        for id in [&q.id, &r.id] {
            let rec = get(&pool, id).await.expect("get").expect("row");
            assert_eq!(rec.state, TaskState::Failed);
            assert_eq!(rec.error_code.as_deref(), Some("TASK_INTERRUPTED"));
        }
        let rec = get(&pool, &done.id).await.expect("get").expect("row");
        assert_eq!(rec.state, TaskState::Succeeded);

        // Idempotent: second boot finds nothing.
        assert_eq!(requeue_interrupted(&pool).await.expect("again"), 0);
    }

    #[tokio::test]
    async fn unknown_task_type_fails_loudly() {
        let pool = test_pool().await;
        let rec = enqueue(&pool, "teleport", None, None).await.expect("q");
        // No daemon needed: unknown types are rejected before any Docker call.
        // (Client constructors perform no I/O.)
        let docker =
            bollard::Docker::connect_with_http_defaults().expect("client builds without I/O");
        let adapter = DockerAdapter::with_docker(docker);
        dispatch_task(&pool, &adapter, &rec.id)
            .await
            .expect("dispatch");
        let r = get(&pool, &rec.id).await.expect("get").expect("row");
        assert_eq!(r.state, TaskState::Failed);
        assert_eq!(r.error_code.as_deref(), Some("UNKNOWN_TASK_TYPE"));
    }
}
