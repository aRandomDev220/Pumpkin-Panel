//! Per-server `pumpkin.toml` management (Phase 4, Task 2).
//!
//! - `GET /servers/:id/config` → current content + guided view + profile,
//!   plus `diverged`/`diverged_reason` when the file disagrees with the
//!   revision baseline (detectable, never auto-healed).
//! - `PUT /servers/:id/config {revision, content}` → revision-checked,
//!   validated, atomic save with a pre-change copy in `config_revisions`.
//! - `POST /servers/:id/config/validate {content}` → dry-run validation.
//! - `GET /servers/:id/config/revisions` → revision history, each entry
//!   carrying its full content for read-only preview (files are ~4KB and
//!   the page limit is capped, so this stays small).
//! - `POST /servers/:id/config/revert {revision}` → old content saved as a
//!   NEW revision through the same transaction.
//!
//! Route wiring + permission layers are the controller's job; every handler
//! here is a standalone `pub async fn` taking `State<AppState>`.
//!
//! Dead-code lints are silenced until the controller wires these handlers
//! into `main.rs` (same transient state as the other Phase 4 task modules).

use std::collections::HashSet;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::{
    audit,
    auth::AuthUser,
    config_profile::{content_hash, guided_from_doc, parse_and_validate, profile_for_version},
    AppState,
};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PutConfigRequest {
    pub revision: i64,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ValidateConfigRequest {
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct RevertConfigRequest {
    pub revision: i64,
}

#[derive(Debug, Deserialize)]
pub struct RevisionsQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
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

struct ServerRow {
    id: String,
    data_path: String,
    runtime_version: String,
}

async fn fetch_server(pool: &SqlitePool, id: &str) -> Result<Option<ServerRow>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id, data_path, runtime_version FROM servers
          WHERE id = ? AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .map(|(id, data_path, runtime_version)| ServerRow {
        id,
        data_path,
        runtime_version,
    });
    Ok(row)
}

fn config_path(server: &ServerRow) -> std::path::PathBuf {
    std::path::Path::new(&server.data_path).join("pumpkin.toml")
}

/// Current `(configuration_revision, content_hash baseline)`, creating the
/// settings row on demand (older rows may predate `server_settings`).
/// `content_hash` is `None` until the first API save populates it (migration
/// 0004) or when the database predates that column — both mean "no baseline".
async fn current_state(pool: &SqlitePool, server_id: &str) -> (i64, Option<String>) {
    let _ = sqlx::query(
        "INSERT INTO server_settings (server_id) VALUES (?)
          ON CONFLICT(server_id) DO NOTHING",
    )
    .bind(server_id)
    .execute(pool)
    .await;
    if let Ok(row) = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT configuration_revision, content_hash FROM server_settings
          WHERE server_id = ?",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    {
        return row;
    }
    // Pre-0004 database: revision without a baseline.
    let revision = sqlx::query_scalar::<_, i64>(
        "SELECT configuration_revision FROM server_settings WHERE server_id = ?",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .unwrap_or(1);
    (revision, None)
}

/// Try to claim the per-server op lock (same pattern as `routes_servers`).
/// `false` = a conflicting save is in flight; the caller must fail fast
/// with `SERVER_BUSY` instead of queuing behind I/O.
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

/// Short hash prefix for human-facing divergence messages (never bodies).
fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

fn profile_json(runtime_version: &str) -> serde_json::Value {
    let profile = profile_for_version(runtime_version);
    serde_json::json!({
        "pumpkin_version": profile.pumpkin_version,
        "known": profile.known,
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/servers/:id/config — current file + guided view + profile.
pub async fn get_config(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Response {
    let Some(server) = fetch_server(&state.pool, &id).await.unwrap_or(None) else {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    };
    let profile = profile_json(&server.runtime_version);
    let content = match tokio::fs::read_to_string(config_path(&server)).await {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Fresh server: Pumpkin never booted, no file yet. The profile
            // is still useful (drives the unknown-version warning).
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error_code": "CONFIG_NOT_FOUND",
                    "message": "pumpkin.toml not present; server has not booted yet",
                    "profile": profile,
                })),
            )
                .into_response();
        }
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to read pumpkin.toml",
            )
        }
    };
    let (revision, baseline) = current_state(&state.pool, &server.id).await;
    let hash = content_hash(&content);
    // Divergence detection: the DB revision row and the on-disk file are
    // dual writes (a crash between rename and bump, or an edit that
    // bypassed this API, leaves them disagreeing). The file is served
    // as-is — never auto-healed — but flagged for the operator.
    let diverged = baseline.as_deref().is_some_and(|b| b != hash);
    let guided = match parse_and_validate(&content) {
        Ok(doc) => guided_from_doc(&doc),
        Err(errors) => {
            // Repair shape: the on-disk TOML is invalid (e.g. broken via a
            // file-manager edit). Return 200 with the raw `content` plus the
            // validation `errors` (`valid: false`) so the editor stays
            // loadable; `guided` is best-effort (all nulls when the file does
            // not even parse). PUT still rejects garbage with 422.
            let guided = content
                .parse::<toml_edit::DocumentMut>()
                .map(|doc| guided_from_doc(&doc))
                .unwrap_or_default();
            let mut body = serde_json::json!({
                "revision": revision,
                "hash": hash,
                "content": content,
                "guided": guided,
                "profile": profile,
                "diverged": diverged,
                "valid": false,
                "errors": errors,
            });
            if diverged {
                body["diverged_reason"] = serde_json::json!(format!(
                    "pumpkin.toml on disk (hash {}) does not match revision {} baseline (hash {}); \
                     edited outside the config API or left behind by an interrupted save — \
                     review and re-save to reconcile",
                    short_hash(&hash),
                    revision,
                    short_hash(baseline.as_deref().unwrap_or("unknown")),
                ));
            }
            return Json(body).into_response();
        }
    };
    let mut body = serde_json::json!({
        "revision": revision,
        "hash": hash,
        "content": content,
        "guided": guided,
        "profile": profile,
        "diverged": diverged,
        "valid": true,
        "errors": [],
    });
    if diverged {
        body["diverged_reason"] = serde_json::json!(format!(
            "pumpkin.toml on disk (hash {}) does not match revision {} baseline (hash {}); \
             edited outside the config API or left behind by an interrupted save — \
             review and re-save to reconcile",
            short_hash(&hash),
            revision,
            short_hash(baseline.as_deref().unwrap_or("unknown")),
        ));
    }
    Json(body).into_response()
}

/// PUT /api/v1/servers/:id/config — revision-checked atomic save.
pub async fn put_config(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<PutConfigRequest>,
) -> Response {
    let Some(server) = fetch_server(&state.pool, &id).await.unwrap_or(None) else {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    };
    save_new_revision(&state, &user, &server, body.revision, body.content).await
}

/// POST /api/v1/servers/:id/config/validate — dry-run validation, never writes.
pub async fn validate_config(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<ValidateConfigRequest>,
) -> Response {
    if fetch_server(&state.pool, &id)
        .await
        .unwrap_or(None)
        .is_none()
    {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    match parse_and_validate(&body.content) {
        Ok(_) => Json(serde_json::json!({ "ok": true, "errors": [] })).into_response(),
        Err(errors) => Json(serde_json::json!({ "ok": false, "errors": errors })).into_response(),
    }
}

/// GET /api/v1/servers/:id/config/revisions — history with per-entry content
/// for read-only preview (amendment).
pub async fn list_revisions(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Query(pager): Query<RevisionsQuery>,
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
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM config_revisions WHERE server_id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
    let rows = sqlx::query(
        "SELECT revision, hash, content, created_at, created_by FROM config_revisions
          WHERE server_id = ? ORDER BY revision DESC LIMIT ? OFFSET ?",
    )
    .bind(&id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await;
    match rows {
        Ok(rows) => {
            use sqlx::Row as _;
            let revisions: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "revision": r.get::<i64, _>("revision"),
                        "hash": r.get::<String, _>("hash"),
                        "content": r.get::<String, _>("content"),
                        "created_at": r.get::<String, _>("created_at"),
                        "created_by": r.get::<Option<String>, _>("created_by"),
                    })
                })
                .collect();
            Json(serde_json::json!({ "revisions": revisions, "total": total })).into_response()
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            "failed to list revisions",
        ),
    }
}

/// POST /api/v1/servers/:id/config/revert — old content saved as a NEW
/// revision through the same transaction (409 on concurrent change).
pub async fn revert_config(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<RevertConfigRequest>,
) -> Response {
    let Some(server) = fetch_server(&state.pool, &id).await.unwrap_or(None) else {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    };
    let row: Option<(String,)> =
        sqlx::query_as("SELECT content FROM config_revisions WHERE server_id = ? AND revision = ?")
            .bind(&server.id)
            .bind(body.revision)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);
    let Some((content,)) = row else {
        return error(
            StatusCode::NOT_FOUND,
            "REVISION_NOT_FOUND",
            "no such config revision",
        );
    };
    let (expected, _) = current_state(&state.pool, &server.id).await;
    save_new_revision(&state, &user, &server, expected, content).await
}

// ---------------------------------------------------------------------------
// Shared save transaction
// ---------------------------------------------------------------------------

/// Entry point for PUT + revert: claims the per-server op lock (fail fast
/// with `409 SERVER_BUSY`, never queue behind another save's I/O), runs the
/// save, then releases the lock. Splitting lock handling from `save_inner`
/// keeps every exit path unlock-safe without `Drop`-based async hacks.
async fn save_new_revision(
    state: &AppState,
    user: &AuthUser,
    server: &ServerRow,
    expected_revision: i64,
    new_content: String,
) -> Response {
    if !try_lock(&state.op_locks, &server.id).await {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another config save is in flight for this server",
        );
    }
    let response = save_inner(state, user, server, expected_revision, new_content).await;
    unlock(&state.op_locks, &server.id).await;
    response
}

/// Revision check → validate → pre-change copy → atomic rename → re-read +
/// hash verify → revision bump (+ baseline hash), under `BEGIN IMMEDIATE` so
/// a save that bypassed the op lock still cannot interleave mid-transaction.
async fn save_inner(
    state: &AppState,
    user: &AuthUser,
    server: &ServerRow,
    expected_revision: i64,
    new_content: String,
) -> Response {
    let request_id = audit::request_id();
    let (current, _) = current_state(&state.pool, &server.id).await;
    if expected_revision != current {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error_code": "CONFIG_CONFLICT",
                "message": "configuration changed since you read it; reload and retry",
                "revision": current,
            })),
        )
            .into_response();
    }
    if let Err(errors) = parse_and_validate(&new_content) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error_code": "INVALID_CONFIGURATION",
                "message": errors.join("; "),
                "errors": errors,
            })),
        )
            .into_response();
    }

    let path = config_path(server);
    let old_content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let old_hash = content_hash(&old_content);
    let new_hash = content_hash(&new_content);
    let next_revision = current + 1;

    // Serialize writers: the second concurrent save blocks here, then fails
    // the re-check below once the first commits.
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "database unavailable",
            )
        }
    };
    if sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *conn)
        .await
        .is_err()
    {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "another config save is in flight; retry",
        );
    }
    let outcome: Result<(), Response> = async {
        let fresh: i64 = sqlx::query_scalar(
            "SELECT configuration_revision FROM server_settings WHERE server_id = ?",
        )
        .bind(&server.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load configuration revision",
            )
        })?;
        if fresh != expected_revision {
            return Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error_code": "CONFIG_CONFLICT",
                    "message": "configuration changed since you read it; reload and retry",
                    "revision": fresh,
                })),
            )
                .into_response());
        }
        sqlx::query(
            "INSERT INTO config_revisions
                (id, server_id, revision, content, hash, created_by)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&server.id)
        .bind(current)
        .bind(&old_content)
        .bind(&old_hash)
        .bind(&user.id)
        .execute(&mut *conn)
        .await
        .map_err(|e| {
            // A duplicate (server_id, revision) row means a concurrent save
            // already stored this pre-change copy: report conflict, not 500.
            if e.to_string().contains("UNIQUE constraint failed") {
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error_code": "CONFIG_CONFLICT",
                        "message": "configuration changed since you read it; reload and retry",
                    })),
                )
                    .into_response()
            } else {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL",
                    "failed to store config revision",
                )
            }
        })?;
        // Tempfile in the same directory + atomic rename: readers never see
        // a half-written pumpkin.toml.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && tokio::fs::create_dir_all(parent).await.is_err() {
                return Err(error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL",
                    "failed to prepare config directory",
                ));
            }
        }
        let tmp = path.with_extension(format!("toml.{}.tmp", uuid::Uuid::new_v4()));
        if let Err(e) = tokio::fs::write(&tmp, new_content.as_bytes()).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            tracing::warn!(server_id = %server.id, error = %e, "config tempfile write failed");
            return Err(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to write pumpkin.toml",
            ));
        }
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            tracing::warn!(server_id = %server.id, error = %e, "config rename failed");
            return Err(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to replace pumpkin.toml",
            ));
        }
        // Re-read + verify: the bytes on disk must hash to what we saved.
        let reread = tokio::fs::read_to_string(&path).await.map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to re-read pumpkin.toml",
            )
        })?;
        if content_hash(&reread) != new_hash {
            return Err(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "config write verification failed",
            ));
        }
        sqlx::query(
            "UPDATE server_settings SET configuration_revision = ?, content_hash = ?,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE server_id = ?",
        )
        .bind(next_revision)
        .bind(&new_hash)
        .bind(&server.id)
        .execute(&mut *conn)
        .await
        .map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to bump configuration revision",
            )
        })?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => {
            let _ = sqlx::query("COMMIT").execute(&mut *conn).await;
        }
        Err(resp) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            return resp;
        }
    }
    // Release the write connection before anything else touches the pool:
    // with a single-connection pool (tests) any further acquire would wait
    // out the acquire timeout and silently drop the audit write below.
    drop(conn);

    // Audit the mutation without bodies: revision + hash only, never content.
    audit::record(
        &state.pool,
        Some(&user.id),
        "server.config.write",
        "server",
        &server.id,
        "ok",
        &request_id,
        serde_json::json!({ "revision": next_revision, "hash": new_hash }),
    )
    .await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "revision": next_revision,
            "hash": new_hash,
            "restart_required": true,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use serde_json::{json, Value};
    use sqlx::SqlitePool;

    use super::*;
    use crate::{build_router, AppState};

    async fn test_pool() -> SqlitePool {
        // Single connection: in-memory SQLite is per-connection.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();
        apply_migrations(&pool).await;
        pool
    }

    async fn apply_migrations(pool: &SqlitePool) {
        for file in [
            include_str!("../../../migrations/0001_init.sql"),
            include_str!("../../../migrations/0002_sessions_audit.sql"),
            include_str!("../../../migrations/0003_config_revisions.sql"),
            include_str!("../../../migrations/0004_config_baseline_hash.sql"),
            include_str!("../../../migrations/0007_update.sql"),
        ] {
            // Strip `--` comment lines first: they may contain semicolons
            // (e.g. "immutable; new tables only") that naive splitting
            // would cut mid-sentence into a bogus statement.
            let stripped: String = file
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            for stmt in stripped.split(';') {
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                sqlx::query(stmt).execute(pool).await.unwrap();
            }
        }
    }

    /// Test app: the full production router (config routes are wired in
    /// `build_router` by the controller with the same permission gates).
    fn test_app(state: AppState) -> axum::Router {
        build_router(state)
    }

    async fn test_server() -> (TestServer, SqlitePool) {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pm-cfg-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState::for_tests(pool.clone(), root);
        let server = TestServer::new(test_app(state)).unwrap();
        (server, pool)
    }

    async fn setup_owner(server: &TestServer) -> String {
        let res = server
            .post("/api/v1/auth/setup")
            .json(&json!({"username": "owner", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200);
        res.json::<Value>()["token"].as_str().unwrap().to_string()
    }

    async fn make_server(server: &TestServer, token: &str, name: &str, port: u16) -> Value {
        let res = server
            .post("/api/v1/servers")
            .authorization_bearer(token)
            .json(&json!({"name": name, "game_port": port,
                          "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        res.json::<Value>()
    }

    fn write_pumpkin_toml(data_path: &str, content: &str) {
        // The API creates the row without materialising the directory when
        // no runtime is attached; tests play the role of a booted Pumpkin.
        let path = std::path::Path::new(data_path);
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("pumpkin.toml"), content).unwrap();
    }

    fn read_pumpkin_toml(data_path: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(data_path).join("pumpkin.toml")).unwrap()
    }

    const V1: &str = "seed = \"abc\"\n[networking.java]\nmotd = \"v1\"\nmax_players = 20\n";
    const V2: &str = "seed = \"abc\"\n[networking.java]\nmotd = \"v2\"\nmax_players = 20\n";
    const GARBAGE: &str = "{{{ not toml [[[ oops";

    async fn revision_of(server: &TestServer, token: &str, id: &str) -> i64 {
        server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(token)
            .await
            .json::<Value>()["revision"]
            .as_i64()
            .unwrap()
    }

    #[tokio::test]
    async fn put_validates_and_rejects_garbage() {
        let (server, _pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-garbage", 26101).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);
        let rev = revision_of(&server, &token, &id).await;

        // Garbage is rejected with 422 and the file is untouched.
        let res = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": GARBAGE}))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
        assert_eq!(
            res.json::<Value>()["error_code"],
            json!("INVALID_CONFIGURATION")
        );
        assert_eq!(read_pumpkin_toml(&data_path), V1);
        assert_eq!(revision_of(&server, &token, &id).await, rev);

        // The dry-run endpoint agrees and never writes either.
        let res = server
            .post(&format!("/api/v1/servers/{id}/config/validate"))
            .authorization_bearer(&token)
            .json(&json!({"content": GARBAGE}))
            .await;
        assert_eq!(res.status_code(), 200);
        let body = res.json::<Value>();
        assert_eq!(body["ok"], json!(false));
        assert!(!body["errors"].as_array().unwrap().is_empty());
        assert_eq!(read_pumpkin_toml(&data_path), V1);

        // Range violations against the real schema are rejected the same way.
        let res = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": "[networking.java]\nmax_players = -5\n"}))
            .await;
        assert_eq!(res.status_code(), 422, "body: {}", res.text());
        assert_eq!(read_pumpkin_toml(&data_path), V1);
    }

    #[tokio::test]
    async fn revision_conflict() {
        let (server, _pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-conflict", 26102).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);
        let rev = revision_of(&server, &token, &id).await;

        // First writer wins.
        let first = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": V2}))
            .await;
        assert_eq!(first.status_code(), 200, "body: {}", first.text());
        assert_eq!(first.json::<Value>()["revision"], json!(rev + 1));

        // Same stale revision loses with 409 CONFIG_CONFLICT.
        let second = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": "motd = \"stale\"\n"}))
            .await;
        assert_eq!(second.status_code(), 409, "body: {}", second.text());
        assert_eq!(
            second.json::<Value>()["error_code"],
            json!("CONFIG_CONFLICT")
        );
        assert_eq!(read_pumpkin_toml(&data_path), V2);
    }

    #[tokio::test]
    async fn revert_restores_content() {
        let (server, _pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-revert", 26103).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);

        // v1 (rev 1, on disk) → PUT v2 (rev 2) → revert to 1 (rev 3).
        let rev = revision_of(&server, &token, &id).await;
        let put = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": V2}))
            .await;
        assert_eq!(put.status_code(), 200, "body: {}", put.text());

        let revert = server
            .post(&format!("/api/v1/servers/{id}/config/revert"))
            .authorization_bearer(&token)
            .json(&json!({"revision": 1}))
            .await;
        assert_eq!(revert.status_code(), 200, "body: {}", revert.text());
        let body = revert.json::<Value>();
        assert_eq!(body["revision"], json!(3));

        let get = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await;
        let get_body = get.json::<Value>();
        assert_eq!(get_body["content"], json!(V1));
        assert_eq!(get_body["revision"], json!(3));
        // Guided projection follows the real schema (nested networking table).
        assert_eq!(get_body["guided"]["motd"], json!("v1"));
        assert_eq!(get_body["guided"]["max_players"], json!(20));
        assert_eq!(read_pumpkin_toml(&data_path), V1);

        // Unknown revision id is 404, not 500.
        let missing = server
            .post(&format!("/api/v1/servers/{id}/config/revert"))
            .authorization_bearer(&token)
            .json(&json!({"revision": 99}))
            .await;
        assert_eq!(missing.status_code(), 404);
        assert_eq!(
            missing.json::<Value>()["error_code"],
            json!("REVISION_NOT_FOUND")
        );
    }

    #[tokio::test]
    async fn unknown_profile_warns() {
        let (server, pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-profile", 26104).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);

        sqlx::query("UPDATE servers SET runtime_version = '99.9' WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        let res = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["profile"]["known"], json!(false));
        assert_eq!(body["profile"]["pumpkin_version"], json!("99.9"));
    }

    #[tokio::test]
    async fn missing_file_returns_config_not_found_with_profile() {
        let (server, pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-fresh", 26105).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();

        // Fresh server, Pumpkin never booted: no pumpkin.toml on disk.
        sqlx::query("UPDATE servers SET runtime_version = '99.9' WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        let res = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(res.status_code(), 404, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["error_code"], json!("CONFIG_NOT_FOUND"));
        assert_eq!(body["profile"]["known"], json!(false));
    }

    /// Proves every step of the save transaction: rev-check → validate →
    /// pre-change copy → atomic rename → re-read → bump → audit.
    #[tokio::test]
    async fn save_transaction_steps() {
        let (server, pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-txn", 26106).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);
        let rev = revision_of(&server, &token, &id).await;

        let res = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": V2}))
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["revision"], json!(rev + 1));
        assert_eq!(body["restart_required"], json!(true));

        // Atomic rename: the file holds exactly the new content, and no
        // tempfile leftovers remain beside it.
        assert_eq!(read_pumpkin_toml(&data_path), V2);
        let mut leftovers = vec![];
        for entry in std::fs::read_dir(&data_path).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name != "pumpkin.toml" {
                leftovers.push(name);
            }
        }
        assert!(leftovers.is_empty(), "temp leftovers: {leftovers:?}");

        // Re-read + hash: the response hash matches the bytes on disk.
        let disk_hash: String = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(read_pumpkin_toml(&data_path).as_bytes());
            format!("{:x}", hasher.finalize())
        };
        assert_eq!(body["hash"], json!(disk_hash));

        // Pre-change copy: revision `rev` stores the OLD content + hash.
        let row: (String, String) = sqlx::query_as(
            "SELECT content, hash FROM config_revisions WHERE server_id = ? AND revision = ?",
        )
        .bind(&id)
        .bind(rev)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, V1);
        assert_eq!(row.1, content_hash(V1));

        // Bump: settings now point at the new revision.
        let bumped: i64 = sqlx::query_scalar(
            "SELECT configuration_revision FROM server_settings WHERE server_id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(bumped, rev + 1);

        // Audit: mutation recorded without the file body.
        let audit_row: (String, String) = sqlx::query_as(
            "SELECT action, metadata FROM audit_events
              WHERE target_id = ? AND action = 'server.config.write'
              ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audit_row.0, "server.config.write");
        assert!(
            !audit_row.1.contains("v1") && !audit_row.1.contains("v2"),
            "audit must not contain file bodies: {}",
            audit_row.1
        );

        // History lists the pre-change copy (bodies stay out of the list).
        let list = server
            .get(&format!("/api/v1/servers/{id}/config/revisions"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(list.status_code(), 200);
        let list_body = list.json::<Value>();
        assert_eq!(list_body["total"], json!(1));
        assert_eq!(list_body["revisions"][0]["revision"], json!(rev));
        // Amendment: the UI previews revision bodies read-only, so each
        // entry carries its full content (files are ~4KB, limit is capped).
        assert_eq!(list_body["revisions"][0]["content"], json!(V1));
    }

    /// Crash/out-of-band divergence is DETECTABLE: the file is served as-is
    /// with `diverged: true` + a human reason, never auto-healed.
    #[tokio::test]
    async fn out_of_band_write_flags_diverged() {
        let (server, _pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-diverged", 26107).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);

        // Establish the baseline via a normal save.
        let rev = revision_of(&server, &token, &id).await;
        let put = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": V2}))
            .await;
        assert_eq!(put.status_code(), 200, "body: {}", put.text());

        // Clean read: file matches the baseline, no divergence.
        let clean = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await
            .json::<Value>();
        assert_eq!(clean["diverged"], json!(false));

        // Out-of-band write (crash between rename and DB bump, or an edit
        // via the file manager that bypassed the revision tracking).
        let tampered = "[networking.java]\nmotd = \"tampered\"\nmax_players = 20\n";
        write_pumpkin_toml(&data_path, tampered);

        let get = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(get.status_code(), 200, "body: {}", get.text());
        let body = get.json::<Value>();
        // Served as-is — never auto-healed — but flagged.
        assert_eq!(body["content"], json!(tampered));
        assert_eq!(body["diverged"], json!(true));
        assert!(
            body["diverged_reason"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "expected a human diverged_reason, got: {body}"
        );
    }

    /// True parallel contention: 8 concurrent PUTs on the same base revision
    /// → exactly one winner (`200`), every loser `409` (immediate
    /// `SERVER_BUSY` while the save is in flight, or `CONFIG_CONFLICT` if it
    /// lands after the winner commits), and exactly one new revision row.
    #[tokio::test]
    async fn parallel_puts_have_single_winner() {
        let (server, pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-parallel", 26108).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);
        let rev = revision_of(&server, &token, &id).await;

        let url = format!("/api/v1/servers/{id}/config");
        let bodies: Vec<Value> = (0..8)
            .map(|i| {
                json!({"revision": rev,
                       "content": format!("seed = \"w{i}\"\n[networking.java]\nmotd = \"w{i}\"\nmax_players = 20\n")})
            })
            .collect();
        let resps = futures_util::future::join_all(bodies.into_iter().map(|b| {
            let req = server.put(&url).authorization_bearer(&token).json(&b);
            async move { req.await }
        }))
        .await;

        let ok = resps.iter().filter(|r| r.status_code() == 200).count();
        assert_eq!(ok, 1, "exactly one winner expected");
        for r in &resps {
            assert!(
                r.status_code() == 200 || r.status_code() == 409,
                "unexpected status {}: {}",
                r.status_code(),
                r.text()
            );
        }
        let busy_or_conflict: Vec<String> = resps
            .iter()
            .filter(|r| r.status_code() == 409)
            .map(|r| r.json::<Value>()["error_code"].to_string())
            .collect();
        assert_eq!(busy_or_conflict.len(), 7);
        for code in &busy_or_conflict {
            assert!(
                code.contains("SERVER_BUSY") || code.contains("CONFIG_CONFLICT"),
                "unexpected 409 code: {code}"
            );
        }

        // Exactly one save took effect: one pre-change row, one bump.
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM config_revisions WHERE server_id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(rows, 1);
        let bumped: i64 = sqlx::query_scalar(
            "SELECT configuration_revision FROM server_settings WHERE server_id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(bumped, rev + 1);
        // The winner's bytes are on disk (one of the eight candidates).
        let disk = read_pumpkin_toml(&data_path);
        assert!(
            disk.contains("motd = \"w"),
            "unexpected disk content: {disk}"
        );
    }

    #[tokio::test]
    async fn empty_version_profile_is_unknown() {
        let (server, _pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-emptyver", 26109).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);
        // `runtime_version` defaults to '' (never reported by the server).
        let res = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["profile"]["known"], json!(false));
        assert_eq!(body["profile"]["pumpkin_version"], json!(""));
    }

    #[tokio::test]
    async fn validate_ok_path_and_revision_pagination() {
        let (server, pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-pages", 26110).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);

        // `ok: true` dry-run path.
        let res = server
            .post(&format!("/api/v1/servers/{id}/config/validate"))
            .authorization_bearer(&token)
            .json(&json!({"content": V1}))
            .await;
        assert_eq!(res.status_code(), 200);
        let body = res.json::<Value>();
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["errors"], json!([]));

        // Seed 105 history rows directly; pagination clamps limit to 100.
        for n in 1..=105i64 {
            sqlx::query(
                "INSERT INTO config_revisions
                    (id, server_id, revision, content, hash)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(format!("rev-{n}"))
            .bind(&id)
            .bind(n)
            .bind(format!("content-{n}"))
            .bind(format!("hash-{n}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        let list = server
            .get(&format!("/api/v1/servers/{id}/config/revisions?limit=1000"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(list.status_code(), 200);
        let list_body = list.json::<Value>();
        assert_eq!(list_body["total"], json!(105));
        assert_eq!(list_body["revisions"].as_array().unwrap().len(), 100);
        // Newest first.
        assert_eq!(list_body["revisions"][0]["revision"], json!(105));

        let page = server
            .get(&format!(
                "/api/v1/servers/{id}/config/revisions?limit=2&offset=1"
            ))
            .authorization_bearer(&token)
            .await
            .json::<Value>();
        assert_eq!(page["revisions"][0]["revision"], json!(104));
        assert_eq!(page["revisions"][1]["revision"], json!(103));
    }

    /// I1: GET on an invalid on-disk TOML returns the 200 repair shape
    /// (`content` + `errors`, `valid: false`) instead of a bare 422, so the
    /// editor stays loadable; PUT garbage still 422s. Also pins the valid
    /// shape (`valid: true`, `errors: []`).
    #[tokio::test]
    async fn get_invalid_toml_returns_repair_shape() {
        let (server, _pool) = test_server().await;
        let token = setup_owner(&server).await;
        let created = make_server(&server, &token, "cfg-repair", 26111).await;
        let id = created["server"]["id"].as_str().unwrap().to_string();
        let data_path = created["server"]["data_path"].as_str().unwrap().to_string();
        write_pumpkin_toml(&data_path, V1);

        // Valid shape first.
        let clean = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(clean.status_code(), 200, "body: {}", clean.text());
        let clean_body = clean.json::<Value>();
        assert_eq!(clean_body["valid"], json!(true));
        assert_eq!(clean_body["errors"], json!([]));

        // Break the file out-of-band (e.g. via a file-manager edit).
        write_pumpkin_toml(&data_path, GARBAGE);
        let rev = revision_of(&server, &token, &id).await;

        let res = server
            .get(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["content"], json!(GARBAGE));
        assert_eq!(body["valid"], json!(false));
        assert!(
            body["errors"].as_array().is_some_and(|e| !e.is_empty()),
            "expected validation errors, got: {body}"
        );
        assert_eq!(body["revision"], json!(rev));
        assert!(body["hash"].is_string(), "got: {body}");
        assert!(body["profile"].is_object(), "got: {body}");
        assert!(body["guided"].is_object(), "got: {body}");

        // PUT garbage still rejects with 422 + leaves the file untouched.
        let put = server
            .put(&format!("/api/v1/servers/{id}/config"))
            .authorization_bearer(&token)
            .json(&json!({"revision": rev, "content": GARBAGE}))
            .await;
        assert_eq!(put.status_code(), 422, "body: {}", put.text());
        assert_eq!(
            put.json::<Value>()["error_code"],
            json!("INVALID_CONFIGURATION")
        );
        assert_eq!(read_pumpkin_toml(&data_path), GARBAGE);
    }
}
