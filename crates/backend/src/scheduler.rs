//! Backup retention policies + the 60s scheduler tick (Phase 5, Task 3).
//!
//! - Policy CRUD handlers (`list/create/update/delete_policy`) and a dry-run
//!   [`retention_preview`]. Route wiring + permission gates are the
//!   controller's job; handlers take `State<AppState>` and assume the caller
//!   enforced `ServerBackupCreate` (writes) / `ServerRead` (reads).
//! - [`tick_once`] scans enabled policies due at `now`, runs one backup per
//!   due policy (single catch-up when overdue), recomputes `next_run_at` from
//!   the cron schedule, and prunes per-policy retention afterwards.
//!
//! Timezone rule (per brief): any non-empty string is accepted and stored,
//! everything is interpreted as UTC, and non-UTC values get
//! `timezone_note: "interpreted as UTC"` in responses.
//!
//! Canonical entry points: cron validation and retention decisions come from
//! `backup_service`; scheduled runs go through `routes_backup::enqueue_backup`
//! (same args the brief froze). Its worker owns stop/resume consistency, the
//! backup semaphore, op-locks, space pre-checks, cancel, task lifecycle, and
//! post-backup prune/audit — this module only schedules.

// Task 2 (routes) / controller wiring consumes this module's entry points;
// nothing in the crate wires it up yet, so dead-code is expected until then
// (remove once wired).

use std::path::Path;
use std::str::FromStr;

use axum::{
    extract::{Path as HttpPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::audit;
use crate::auth::AuthUser;
use crate::backup_service::{cron_is_valid, select_for_deletion, RetentionInput};
use crate::routes_backup::{enqueue_backup, BackupError};
use crate::runtime::DockerAdapter;
use crate::AppState;

/// Returned in policy responses whenever `timezone != "UTC"`.
pub const TIMEZONE_NOTE_UTC: &str = "interpreted as UTC";

/// `backup_policies` DDL, verbatim from the Phase 5 plan (Task 2 migration
/// text), plus `IF NOT EXISTS` so `tick_once` and handlers are safe to run in
/// any order relative to that migration.
const BACKUP_POLICIES_DDL: &str = "CREATE TABLE IF NOT EXISTS backup_policies (id TEXT PRIMARY KEY, server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE, name TEXT NOT NULL DEFAULT '', schedule TEXT NOT NULL DEFAULT '', timezone TEXT NOT NULL DEFAULT 'UTC', retention_count INTEGER NOT NULL DEFAULT 7, max_age_days INTEGER NOT NULL DEFAULT 30, enabled INTEGER NOT NULL DEFAULT 1, last_run_at TEXT, next_run_at TEXT, created_by TEXT REFERENCES users(id), created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')), updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')))";

/// `backups` DDL, verbatim from the Phase 5 plan (Task 2 migration text),
/// plus `IF NOT EXISTS` for the same ordering safety.
const BACKUPS_DDL: &str = "CREATE TABLE IF NOT EXISTS backups (id TEXT PRIMARY KEY, server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE, type TEXT NOT NULL DEFAULT 'full', state TEXT NOT NULL DEFAULT 'in_progress' CHECK (state IN ('in_progress','completed','failed')), storage_path TEXT NOT NULL DEFAULT '', size_bytes INTEGER NOT NULL DEFAULT 0, checksum_algorithm TEXT NOT NULL DEFAULT 'sha256', checksum TEXT NOT NULL DEFAULT '', consistency_method TEXT NOT NULL DEFAULT 'stopped', created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')), completed_at TEXT, created_by TEXT REFERENCES users(id), error_summary TEXT, protected INTEGER NOT NULL DEFAULT 0)";

/// Ensure the tables this module reads/writes exist. Idempotent; safe to call
/// on every tick and from every handler.
async fn ensure_tables(pool: &SqlitePool) {
    for ddl in [BACKUP_POLICIES_DDL, BACKUPS_DDL] {
        if let Err(e) = sqlx::query(ddl).execute(pool).await {
            tracing::warn!(error = %e, "scheduler table ensure failed");
        }
    }
    if let Err(e) =
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_backups_server ON backups(server_id)")
            .execute(pool)
            .await
    {
        tracing::warn!(error = %e, "scheduler index ensure failed");
    }
}

// ---------------------------------------------------------------------------
// Cron helpers (validation is canonical `backup_service::cron_is_valid`)
// ---------------------------------------------------------------------------

/// Parse a cron expression for next-run arithmetic only. The `cron` crate
/// parses 6-field (seconds first) or 7-field (with year) schedules, so a
/// 5-field standard expression is shimmed with a `0` seconds field.
fn parse_schedule(expr: &str) -> Option<cron::Schedule> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }
    let fields = expr.split_whitespace().count();
    let shimmed;
    let effective = if fields == 5 {
        shimmed = format!("0 {expr}");
        shimmed.as_str()
    } else if fields == 6 || fields == 7 {
        expr
    } else {
        return None;
    };
    cron::Schedule::from_str(effective).ok()
}

/// Next occurrence strictly after `from`, interpreted as UTC.
pub fn next_run_after(expr: &str, from: DateTime<Utc>) -> Option<DateTime<Utc>> {
    parse_schedule(expr)?.after(&from).next()
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// `Some("interpreted as UTC")` for non-`UTC` timezones, else `None`.
fn timezone_note(timezone: &str) -> Option<&'static str> {
    if timezone == "UTC" {
        None
    } else {
        Some(TIMEZONE_NOTE_UTC)
    }
}

// ---------------------------------------------------------------------------
// Policy rows / responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct PolicyRow {
    id: String,
    server_id: String,
    name: String,
    schedule: String,
    timezone: String,
    retention_count: i64,
    max_age_days: i64,
    enabled: i64,
    last_run_at: Option<String>,
    next_run_at: Option<String>,
    created_by: Option<String>,
    created_at: String,
    updated_at: String,
}

fn policy_json(p: &PolicyRow) -> serde_json::Value {
    serde_json::json!({
        "id": p.id,
        "server_id": p.server_id,
        "name": p.name,
        "schedule": p.schedule,
        "timezone": p.timezone,
        "timezone_note": timezone_note(&p.timezone),
        "retention_count": p.retention_count,
        "max_age_days": p.max_age_days,
        "enabled": p.enabled != 0,
        "last_run_at": p.last_run_at,
        "next_run_at": p.next_run_at,
        "created_by": p.created_by,
        "created_at": p.created_at,
        "updated_at": p.updated_at,
    })
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

async fn server_exists(pool: &SqlitePool, server_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM servers WHERE id = ? AND deleted_at IS NULL")
        .bind(server_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
        > 0
}

async fn fetch_policy(pool: &SqlitePool, policy_id: &str) -> Option<PolicyRow> {
    sqlx::query_as::<_, PolicyRow>(
        "SELECT id, server_id, name, schedule, timezone, retention_count,
                max_age_days, enabled, last_run_at, next_run_at, created_by,
                created_at, updated_at
         FROM backup_policies WHERE id = ?",
    )
    .bind(policy_id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None)
}

// ---------------------------------------------------------------------------
// Handlers (controller wires routes + permission gates)
// ---------------------------------------------------------------------------

/// `GET /servers/:id/backup-policies` → `{policies}`.
pub async fn list_policies(
    State(state): State<AppState>,
    _user: AuthUser,
    HttpPath(server_id): HttpPath<String>,
) -> Response {
    ensure_tables(&state.pool).await;
    if !server_exists(&state.pool, &server_id).await {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    let rows = sqlx::query_as::<_, PolicyRow>(
        "SELECT id, server_id, name, schedule, timezone, retention_count,
                max_age_days, enabled, last_run_at, next_run_at, created_by,
                created_at, updated_at
         FROM backup_policies WHERE server_id = ? ORDER BY created_at ASC",
    )
    .bind(&server_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let policies: Vec<serde_json::Value> = rows.iter().map(policy_json).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "policies": policies })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct CreatePolicyBody {
    #[serde(default)]
    pub name: String,
    pub schedule: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default = "default_retention_count")]
    pub retention_count: i64,
    #[serde(default = "default_max_age_days")]
    pub max_age_days: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_retention_count() -> i64 {
    7
}

fn default_max_age_days() -> i64 {
    30
}

fn default_enabled() -> bool {
    true
}

/// `POST /servers/:id/backup-policies` → `201 {policy}`.
pub async fn create_policy(
    State(state): State<AppState>,
    user: AuthUser,
    HttpPath(server_id): HttpPath<String>,
    Json(body): Json<CreatePolicyBody>,
) -> Response {
    ensure_tables(&state.pool).await;
    if !server_exists(&state.pool, &server_id).await {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    if !cron_is_valid(&body.schedule) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_SCHEDULE",
            "schedule is not a valid 5-field cron expression",
        );
    }
    let timezone = body.timezone.trim();
    if timezone.is_empty() {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_TIMEZONE",
            "timezone must be a non-empty string (interpreted as UTC)",
        );
    }
    if body.retention_count < 0 || body.max_age_days < 0 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_RETENTION",
            "retention_count and max_age_days must be >= 0",
        );
    }
    let now = Utc::now();
    let next_run_at = next_run_after(body.schedule.trim(), now)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let id = uuid::Uuid::new_v4().to_string();
    let now_str = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let insert = sqlx::query(
        "INSERT INTO backup_policies
            (id, server_id, name, schedule, timezone, retention_count,
             max_age_days, enabled, next_run_at, created_by, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&server_id)
    .bind(body.name.trim())
    .bind(body.schedule.trim())
    .bind(timezone)
    .bind(body.retention_count)
    .bind(body.max_age_days)
    .bind(if body.enabled { 1 } else { 0 })
    .bind(&next_run_at)
    .bind(&user.id)
    .bind(&now_str)
    .bind(&now_str)
    .execute(&state.pool)
    .await;
    if let Err(e) = insert {
        tracing::warn!(error = %e, "create_policy insert failed");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to create policy",
        );
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "backup.policy.create",
        "backup_policy",
        &id,
        "ok",
        &audit::request_id(),
        serde_json::json!({ "server_id": server_id, "schedule": body.schedule.trim() }),
    )
    .await;
    match fetch_policy(&state.pool, &id).await {
        Some(row) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "policy": policy_json(&row) })),
        )
            .into_response(),
        None => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "policy created but not readable",
        ),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct UpdatePolicyBody {
    pub name: Option<String>,
    pub schedule: Option<String>,
    pub timezone: Option<String>,
    pub retention_count: Option<i64>,
    pub max_age_days: Option<i64>,
    pub enabled: Option<bool>,
}

/// `PATCH /backup-policies/:id` → `{policy}`.
pub async fn update_policy(
    State(state): State<AppState>,
    user: AuthUser,
    HttpPath(policy_id): HttpPath<String>,
    Json(body): Json<UpdatePolicyBody>,
) -> Response {
    ensure_tables(&state.pool).await;
    let Some(existing) = fetch_policy(&state.pool, &policy_id).await else {
        return error(
            StatusCode::NOT_FOUND,
            "POLICY_NOT_FOUND",
            "no such backup policy",
        );
    };
    let name = body
        .name
        .as_deref()
        .unwrap_or(&existing.name)
        .trim()
        .to_string();
    let schedule = body
        .schedule
        .as_deref()
        .unwrap_or(&existing.schedule)
        .trim()
        .to_string();
    let timezone = body
        .timezone
        .as_deref()
        .unwrap_or(&existing.timezone)
        .trim()
        .to_string();
    let retention_count = body.retention_count.unwrap_or(existing.retention_count);
    let max_age_days = body.max_age_days.unwrap_or(existing.max_age_days);
    let enabled = body.enabled.unwrap_or(existing.enabled != 0);

    if !cron_is_valid(&schedule) {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_SCHEDULE",
            "schedule is not a valid 5-field cron expression",
        );
    }
    if timezone.is_empty() {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_TIMEZONE",
            "timezone must be a non-empty string (interpreted as UTC)",
        );
    }
    if retention_count < 0 || max_age_days < 0 {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_RETENTION",
            "retention_count and max_age_days must be >= 0",
        );
    }
    // Recompute the next run whenever the schedule changed.
    let next_run_at = if schedule != existing.schedule {
        next_run_after(&schedule, Utc::now())
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    } else {
        existing.next_run_at.clone()
    };
    let now_str = now_rfc3339();
    if let Err(e) = sqlx::query(
        "UPDATE backup_policies SET name = ?, schedule = ?, timezone = ?,
                retention_count = ?, max_age_days = ?, enabled = ?,
                next_run_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&schedule)
    .bind(&timezone)
    .bind(retention_count)
    .bind(max_age_days)
    .bind(if enabled { 1 } else { 0 })
    .bind(&next_run_at)
    .bind(&now_str)
    .bind(&policy_id)
    .execute(&state.pool)
    .await
    {
        tracing::warn!(error = %e, "update_policy failed");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to update policy",
        );
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "backup.policy.update",
        "backup_policy",
        &policy_id,
        "ok",
        &audit::request_id(),
        serde_json::json!({ "server_id": existing.server_id }),
    )
    .await;
    match fetch_policy(&state.pool, &policy_id).await {
        Some(row) => (
            StatusCode::OK,
            Json(serde_json::json!({ "policy": policy_json(&row) })),
        )
            .into_response(),
        None => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "policy updated but not readable",
        ),
    }
}

/// `DELETE /backup-policies/:id` → `204`.
pub async fn delete_policy(
    State(state): State<AppState>,
    user: AuthUser,
    HttpPath(policy_id): HttpPath<String>,
) -> Response {
    ensure_tables(&state.pool).await;
    let Some(existing) = fetch_policy(&state.pool, &policy_id).await else {
        return error(
            StatusCode::NOT_FOUND,
            "POLICY_NOT_FOUND",
            "no such backup policy",
        );
    };
    if let Err(e) = sqlx::query("DELETE FROM backup_policies WHERE id = ?")
        .bind(&policy_id)
        .execute(&state.pool)
        .await
    {
        tracing::warn!(error = %e, "delete_policy failed");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to delete policy",
        );
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "backup.policy.delete",
        "backup_policy",
        &policy_id,
        "ok",
        &audit::request_id(),
        serde_json::json!({ "server_id": existing.server_id }),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Retention preview (dry-run)
// ---------------------------------------------------------------------------

/// Completed backups as canonical Task 1 retention inputs (newest first),
/// plus the protected count for the preview response.
async fn load_completed_inputs(pool: &SqlitePool, server_id: &str) -> (Vec<RetentionInput>, usize) {
    let rows = sqlx::query_as::<_, (String, Option<String>, String, String, i64)>(
        "SELECT id, completed_at, created_at, checksum, protected FROM backups
         WHERE server_id = ? AND state = 'completed'
         ORDER BY COALESCE(completed_at, created_at) DESC",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let protected_kept = rows.iter().filter(|r| r.4 != 0).count();
    let inputs = rows
        .into_iter()
        .map(
            |(id, completed_at, created_at, checksum, protected)| RetentionInput {
                id,
                completed_at: completed_at.or(Some(created_at)),
                verified: !checksum.is_empty(),
                protected: protected != 0,
                in_progress: false,
            },
        )
        .collect();
    (inputs, protected_kept)
}

/// Union of per-policy deletion sets over the server's completed backups
/// using the canonical Task 1 decision fn. Pure read path for the preview
/// handler (post-backup prune/audit is owned by Task 2's worker).
async fn compute_would_delete(pool: &SqlitePool, server_id: &str) -> (Vec<String>, usize, usize) {
    let (inputs, protected_kept) = load_completed_inputs(pool, server_id).await;
    let policies = sqlx::query_as::<_, (i64, i64)>(
        "SELECT retention_count, max_age_days FROM backup_policies
         WHERE server_id = ? AND enabled != 0 ORDER BY created_at ASC",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    // No enabled policy → fall back to the column defaults (keep 7 / 30d).
    let policies: Vec<(i64, i64)> = if policies.is_empty() {
        vec![(7, 30)]
    } else {
        policies
    };
    let now_unix = Utc::now().timestamp().max(0) as u64;
    let mut union: Vec<String> = Vec::new();
    for (keep, age) in policies {
        let keep = keep.max(0) as usize;
        let age = age.max(0) as u64;
        for id in select_for_deletion(&inputs, keep, age, now_unix) {
            if !union.contains(&id) {
                union.push(id);
            }
        }
    }
    let retained = inputs.len().saturating_sub(union.len());
    (union, retained, protected_kept)
}

/// `POST /servers/:id/backup-policies/preview`
/// → `{would_delete, retained, protected_kept}`. Dry-run: writes nothing.
pub async fn retention_preview(
    State(state): State<AppState>,
    _user: AuthUser,
    HttpPath(server_id): HttpPath<String>,
) -> Response {
    ensure_tables(&state.pool).await;
    if !server_exists(&state.pool, &server_id).await {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    let (would_delete, retained, protected_kept) =
        compute_would_delete(&state.pool, &server_id).await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "would_delete": would_delete,
            "retained": retained,
            "protected_kept": protected_kept,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Scheduler tick
// ---------------------------------------------------------------------------

/// One scheduler pass. The controller calls this every 60s.
///
/// For each enabled policy with `next_run_at <= now` (or `NULL`): trigger one
/// backup via Task 2's `enqueue_backup` (single catch-up when overdue,
/// audited), recompute `next_run_at` from cron, and skip with reason when
/// disabled mid-flight / server deleted / the worker reports `SERVER_BUSY`.
/// Concurrency, stop/resume consistency, prune, and worker audit are owned by
/// Task 2's worker — the tick never runs backups itself.
pub async fn tick_once(pool: &SqlitePool, adapter: Option<&DockerAdapter>, backup_root: &Path) {
    // Archive storage paths are owned by Task 2's worker (`backup_root_for`);
    // the parameter stays for the briefed controller signature.
    let _ = backup_root;
    ensure_tables(pool).await;
    let now = Utc::now();
    let now_str = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    let due = sqlx::query_as::<_, PolicyRow>(
        "SELECT id, server_id, name, schedule, timezone, retention_count,
                max_age_days, enabled, last_run_at, next_run_at, created_by,
                created_at, updated_at
         FROM backup_policies
         WHERE enabled != 0 AND (next_run_at IS NULL OR next_run_at <= ?)
         ORDER BY next_run_at ASC",
    )
    .bind(&now_str)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for policy in due {
        // Re-check liveness mid-flight: disabled or server gone → skip.
        let fresh = fetch_policy(pool, &policy.id).await;
        let Some(fresh) = fresh else {
            continue;
        };
        if fresh.enabled == 0 {
            audit::record(
                pool,
                fresh.created_by.as_deref(),
                "backup.schedule.skipped",
                "backup_policy",
                &fresh.id,
                "skipped",
                &audit::request_id(),
                serde_json::json!({ "server_id": fresh.server_id, "reason": "POLICY_DISABLED" }),
            )
            .await;
            continue;
        }
        if !server_exists(pool, &fresh.server_id).await {
            push_next_run(pool, &fresh, &now).await;
            audit::record(
                pool,
                fresh.created_by.as_deref(),
                "backup.schedule.skipped",
                "backup_policy",
                &fresh.id,
                "skipped",
                &audit::request_id(),
                serde_json::json!({ "server_id": fresh.server_id, "reason": "SERVER_DELETED" }),
            )
            .await;
            continue;
        }

        // Single catch-up: trigger once no matter how overdue, then schedule
        // the next run strictly after now (never chain multiple runs).
        let catch_up = fresh
            .next_run_at
            .as_deref()
            .and_then(parse_time)
            .is_some_and(|t| (now - t).num_seconds() > 120);

        let requested_by = fresh.created_by.clone();
        match enqueue_backup(
            pool,
            adapter,
            &fresh.server_id,
            requested_by.as_deref(),
            "stopped",
        )
        .await
        {
            Ok((task_id, backup_id)) => {
                let _ = sqlx::query("UPDATE backup_policies SET last_run_at = ? WHERE id = ?")
                    .bind(&now_str)
                    .bind(&fresh.id)
                    .execute(pool)
                    .await;
                push_next_run(pool, &fresh, &now).await;
                audit::record(
                    pool,
                    requested_by.as_deref(),
                    "backup.scheduled",
                    "backup",
                    &backup_id,
                    "ok",
                    &audit::request_id(),
                    serde_json::json!({
                        "server_id": fresh.server_id,
                        "policy_id": fresh.id,
                        "task_id": task_id,
                        "catch_up": catch_up,
                    }),
                )
                .await;
            }
            Err(BackupError::ServerBusy) => {
                push_next_run(pool, &fresh, &now).await;
                audit::record(
                    pool,
                    requested_by.as_deref(),
                    "backup.schedule.skipped",
                    "backup_policy",
                    &fresh.id,
                    "skipped",
                    &audit::request_id(),
                    serde_json::json!({ "server_id": fresh.server_id, "reason": "SERVER_BUSY" }),
                )
                .await;
            }
            Err(BackupError::ServerNotFound) => {
                // Lost a race with server deletion after the pre-check.
                push_next_run(pool, &fresh, &now).await;
                audit::record(
                    pool,
                    requested_by.as_deref(),
                    "backup.schedule.skipped",
                    "backup_policy",
                    &fresh.id,
                    "skipped",
                    &audit::request_id(),
                    serde_json::json!({ "server_id": fresh.server_id, "reason": "SERVER_DELETED" }),
                )
                .await;
            }
            Err(e) => {
                let _ = sqlx::query("UPDATE backup_policies SET last_run_at = ? WHERE id = ?")
                    .bind(&now_str)
                    .bind(&fresh.id)
                    .execute(pool)
                    .await;
                push_next_run(pool, &fresh, &now).await;
                audit::record(
                    pool,
                    requested_by.as_deref(),
                    "backup.scheduled",
                    "backup_policy",
                    &fresh.id,
                    "failed",
                    &audit::request_id(),
                    serde_json::json!({
                        "server_id": fresh.server_id,
                        "catch_up": catch_up,
                        "error_code": e.code(),
                        "error": e.to_string().chars().take(300).collect::<String>(),
                    }),
                )
                .await;
            }
        }
    }
}

/// Recompute `next_run_at` strictly after `now`. On an unparseable schedule
/// (should not happen — validated at write time) push one hour out so one bad
/// row cannot spin the tick loop.
async fn push_next_run(pool: &SqlitePool, policy: &PolicyRow, now: &DateTime<Utc>) {
    let next = next_run_after(&policy.schedule, *now)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let next = next.unwrap_or_else(|| {
        (*now + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    });
    let _ = sqlx::query("UPDATE backup_policies SET next_run_at = ? WHERE id = ?")
        .bind(&next)
        .bind(&policy.id)
        .execute(pool)
        .await;
}

// ---------------------------------------------------------------------------
// Tests (TDD: written first, watched fail against the stub, then implemented)
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum_test::TestServer;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    async fn test_pool() -> SqlitePool {
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
        ensure_tables(&pool).await;
        pool
    }

    /// Full production router (policy routes are wired in `build_router`
    /// by the controller with the same permission gates).
    fn test_app(state: AppState) -> axum::Router {
        crate::build_router(state)
    }

    async fn test_server_with_roots() -> (TestServer, SqlitePool, PathBuf, PathBuf) {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pm-sched-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let backup_root = root.join("backups");
        std::fs::create_dir_all(&backup_root).unwrap();
        let state = AppState::for_tests(pool.clone(), root.clone());
        (
            TestServer::new(test_app(state)).unwrap(),
            pool,
            root,
            backup_root,
        )
    }

    async fn owner_token(server: &TestServer) -> String {
        let res = server
            .post("/api/v1/auth/setup")
            .json(&json!({"username": "owner", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        res.json::<Value>()["token"].as_str().unwrap().to_string()
    }

    async fn make_server(server: &TestServer, token: &str, name: &str, port: u16) -> String {
        let res = server
            .post("/api/v1/servers")
            .authorization_bearer(token)
            .json(&json!({"name": name, "game_port": port,
                          "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        res.json::<Value>()["server"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn data_path_of(pool: &SqlitePool, server_id: &str) -> PathBuf {
        let row: (String,) = sqlx::query_as("SELECT data_path FROM servers WHERE id = ?")
            .bind(server_id)
            .fetch_one(pool)
            .await
            .unwrap();
        PathBuf::from(row.0)
    }

    async fn seed_completed_backup(
        pool: &SqlitePool,
        server_id: &str,
        completed_at: &str,
        protected: bool,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO backups
                (id, server_id, type, state, storage_path, size_bytes,
                 checksum_algorithm, checksum, consistency_method,
                 created_at, completed_at, protected)
             VALUES (?, ?, 'full', 'completed', '', 100, 'sha256', 'abc123', 'stopped', ?, ?, ?)",
        )
        .bind(&id)
        .bind(server_id)
        .bind(completed_at)
        .bind(completed_at)
        .bind(if protected { 1 } else { 0 })
        .execute(pool)
        .await
        .unwrap();
        id
    }

    // -- pure unit tests ----------------------------------------------------

    #[test]
    fn cron_shim_accepts_five_fields_rejects_garbage() {
        assert!(cron_is_valid("0 2 * * *"));
        assert!(cron_is_valid("*/5 * * * *"));
        assert!(!cron_is_valid("sometimes"));
        assert!(!cron_is_valid(""));
        assert!(!cron_is_valid("0 2 * *"));
    }

    #[test]
    fn retention_floor_keeps_last_verified() {
        let mk = |id: &str, verified: bool, protected: bool, in_progress: bool| RetentionInput {
            id: id.to_string(),
            completed_at: Some("2026-01-0%dT00:00:00.000Z".replace("%d", id)),
            verified,
            protected,
            in_progress,
        };
        // keep_count=0 still keeps the single verified backup.
        let rows = vec![mk("3", true, false, false)];
        assert!(select_for_deletion(&rows, 0, 0, 9_999_999_999).is_empty());
        // Protected + in-progress are never listed; the protected verified
        // row already satisfies the floor, so the spare unprotected row goes.
        let rows = vec![
            mk("3", true, true, false),
            mk("2", true, false, true),
            mk("1", true, false, false),
        ];
        assert_eq!(
            select_for_deletion(&rows, 0, 0, 9_999_999_999),
            vec!["1".to_string()]
        );
        // keep 1 of 3 verified → oldest 2 listed... floor spares nothing here
        // (newest verified survives).
        let rows = vec![
            mk("3", true, false, false),
            mk("2", true, false, false),
            mk("1", true, false, false),
        ];
        let mut doomed = select_for_deletion(&rows, 1, 0, 9_999_999_999);
        doomed.sort();
        assert_eq!(doomed, vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn retention_protected_consumes_keep_slot() {
        // Canonical Task 1 semantics: keep_count ranks among non-in-progress
        // rows, so a protected newest row consumes the keep slot and BOTH
        // unprotected rows go (floor still holds: protected P survives).
        let mk = |id: &str, protected: bool| RetentionInput {
            id: id.to_string(),
            completed_at: Some("2026-01-0%dT00:00:00.000Z".replace("%d", id)),
            verified: true,
            protected,
            in_progress: false,
        };
        let rows = vec![mk("P", true), mk("B", false), mk("C", false)];
        assert_eq!(
            select_for_deletion(&rows, 1, 0, 9_999_999_999),
            vec!["B".to_string(), "C".to_string()]
        );
    }

    // -- handler tests ------------------------------------------------------

    #[tokio::test]
    async fn bad_cron_rejected() {
        let (server, _pool, _root, _backups) = test_server_with_roots().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "cron1", 25565).await;
        let res = server
            .post(&format!("/api/v1/servers/{id}/backup-policies"))
            .authorization_bearer(&t)
            .json(&json!({
                "name": "nightly", "schedule": "sometimes",
                "timezone": "UTC", "retention_count": 7,
                "max_age_days": 30, "enabled": true,
            }))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
        assert_eq!(res.json::<Value>()["error_code"], json!("INVALID_SCHEDULE"));

        // The documented example schedule is accepted.
        let res = server
            .post(&format!("/api/v1/servers/{id}/backup-policies"))
            .authorization_bearer(&t)
            .json(&json!({
                "name": "nightly", "schedule": "0 2 * * *",
                "timezone": "UTC", "retention_count": 7,
                "max_age_days": 30, "enabled": true,
            }))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
    }

    #[tokio::test]
    async fn non_utc_timezone_gets_note() {
        let (server, _pool, _root, _backups) = test_server_with_roots().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "tz1", 25566).await;
        let res = server
            .post(&format!("/api/v1/servers/{id}/backup-policies"))
            .authorization_bearer(&t)
            .json(&json!({
                "name": "tz", "schedule": "0 2 * * *",
                "timezone": "America/New_York", "retention_count": 7,
                "max_age_days": 30, "enabled": true,
            }))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["policy"]["timezone_note"], json!("interpreted as UTC"));
    }

    #[tokio::test]
    async fn preview_is_dry_run() {
        let (server, pool, _root, _backups) = test_server_with_roots().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "prev1", 25567).await;
        for (i, day) in ["01", "02", "03"].iter().enumerate() {
            let _ = i;
            seed_completed_backup(&pool, &id, &format!("2026-01-{day}T00:00:00.000Z"), false).await;
        }
        // Policy keeps 1 → preview must list the 2 oldest.
        let res = server
            .post(&format!("/api/v1/servers/{id}/backup-policies"))
            .authorization_bearer(&t)
            .json(&json!({
                "name": "keep1", "schedule": "0 2 * * *",
                "timezone": "UTC", "retention_count": 1,
                "max_age_days": 0, "enabled": true,
            }))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());

        let res = server
            .post(&format!("/api/v1/servers/{id}/backup-policies/preview"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["would_delete"].as_array().unwrap().len(), 2);
        assert_eq!(body["retained"], json!(1));
        assert_eq!(body["protected_kept"], json!(0));

        // Dry-run: all 3 rows still present.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM backups WHERE server_id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 3);
    }

    async fn poll_backup_task(pool: &SqlitePool, server_id: &str) -> (String, Option<String>) {
        for _ in 0..150 {
            let row: Option<(String, Option<String>)> = sqlx::query_as(
                "SELECT state, error_code FROM tasks
                 WHERE server_id = ? AND type = 'backup' ORDER BY rowid DESC LIMIT 1",
            )
            .bind(server_id)
            .fetch_optional(pool)
            .await
            .unwrap();
            if let Some((state, code)) = row {
                if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
                    return (state, code);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("backup task did not reach a terminal state");
    }

    async fn insert_due_policy(pool: &SqlitePool, server_id: &str) -> String {
        let past = (Utc::now() - chrono::Duration::hours(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let policy_id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO backup_policies
                (id, server_id, name, schedule, timezone, retention_count,
                 max_age_days, enabled, next_run_at, created_at, updated_at)
             VALUES (?, ?, 'nightly', '0 2 * * *', 'UTC', 7, 30, 1, ?, ?, ?)",
        )
        .bind(&policy_id)
        .bind(server_id)
        .bind(&past)
        .bind(&past)
        .bind(&past)
        .execute(pool)
        .await
        .unwrap();
        policy_id
    }

    // -- tick tests ---------------------------------------------------------

    /// SWAP 3 pin: a scheduled run with `consistency="stopped"` on a
    /// container-backed server WITHOUT a runtime must fail honestly
    /// (`RUNTIME_UNAVAILABLE`), never record a silent "success".
    #[tokio::test]
    async fn scheduled_run_without_runtime_fails_honest() {
        let (server, pool, _root, backup_root) = test_server_with_roots().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "tick-noruntime", 25571).await;
        let data_dir = data_path_of(&pool, &id).await;
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("level.dat"), b"data").unwrap();
        // Container-backed: a daemon would have to stop this first.
        sqlx::query("UPDATE servers SET container_id = ? WHERE id = ?")
            .bind("cid-without-daemon")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        insert_due_policy(&pool, &id).await;

        tick_once(&pool, None, &backup_root).await;
        let (state, code) = poll_backup_task(&pool, &id).await;
        assert_eq!(state, "failed");
        assert_eq!(code.as_deref(), Some("RUNTIME_UNAVAILABLE"));

        // No completed backup row may exist for the dishonest run.
        let completed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM backups WHERE server_id = ? AND state = 'completed'",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(completed, 0);
    }

    #[tokio::test]
    async fn tick_runs_due_policy() {
        let (server, pool, _root, backup_root) = test_server_with_roots().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "tick1", 25568).await;
        // Server with real files, no daemon attached.
        let data_dir = data_path_of(&pool, &id).await;
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("level.dat"), b"hello-minecraft").unwrap();
        insert_due_policy(&pool, &id).await;

        tick_once(&pool, None, &backup_root).await;

        // The tick only enqueues; Task 2's worker completes the backup.
        let (state, code) = poll_backup_task(&pool, &id).await;
        assert_eq!(state, "succeeded", "code: {code:?}");
        let row: Option<(String, String, i64)> =
            sqlx::query_as("SELECT state, checksum, size_bytes FROM backups WHERE server_id = ?")
                .bind(&id)
                .fetch_optional(&pool)
                .await
                .unwrap();
        let (bstate, checksum, size) = row.expect("scheduled backup row missing");
        assert_eq!(bstate, "completed");
        assert!(!checksum.is_empty());
        assert!(size > 0);
        // next_run_at pushed into the future (single catch-up, no chaining).
        let next: String =
            sqlx::query_scalar("SELECT next_run_at FROM backup_policies WHERE server_id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            parse_time(&next).unwrap() > Utc::now(),
            "next_run_at not pushed: {next}"
        );
    }

    #[tokio::test]
    async fn tick_skips_when_busy() {
        let (server, pool, _root, backup_root) = test_server_with_roots().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "tick2", 25569).await;
        let data_dir = data_path_of(&pool, &id).await;
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("a.txt"), b"x").unwrap();
        let policy_id = insert_due_policy(&pool, &id).await;

        // A queued backup task claims the server.
        sqlx::query(
            "INSERT INTO tasks (id, type, server_id, state) VALUES (?, 'backup', ?, 'queued')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();

        tick_once(&pool, None, &backup_root).await;

        // `enqueue_backup` rejects before creating any row: no new backup,
        // no new task, next_run pushed, busy skip audited.
        let backups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM backups WHERE server_id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(backups, 0);
        let tasks: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE server_id = ? AND type = 'backup'",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tasks, 1);
        let next: String =
            sqlx::query_scalar("SELECT next_run_at FROM backup_policies WHERE id = ?")
                .bind(&policy_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            parse_time(&next).unwrap() > Utc::now(),
            "next_run_at not pushed: {next}"
        );
        let skips: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events WHERE action = 'backup.schedule.skipped'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(skips >= 1);
    }

    #[tokio::test]
    async fn tick_skips_deleted_server() {
        let (server, pool, _root, backup_root) = test_server_with_roots().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "tick3", 25570).await;
        let past = (Utc::now() - chrono::Duration::hours(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        sqlx::query(
            "INSERT INTO backup_policies
                (id, server_id, name, schedule, timezone, retention_count,
                 max_age_days, enabled, next_run_at, created_at, updated_at)
             VALUES (?, ?, 'nightly', '0 2 * * *', 'UTC', 7, 30, 1, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .bind(&past)
        .bind(&past)
        .bind(&past)
        .execute(&pool)
        .await
        .unwrap();
        // Server deleted mid-flight.
        sqlx::query("UPDATE servers SET deleted_at = ? WHERE id = ?")
            .bind(&past)
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();

        tick_once(&pool, None, &backup_root).await;

        let backups: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM backups")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(backups, 0);
    }
}
