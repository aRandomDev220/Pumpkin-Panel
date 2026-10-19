//! Per-server file manager endpoints (Phase 4, Task 4).
//!
//! Contracts (frozen with `apps/web/src/lib/files.ts`):
//! - `GET /servers/:id/files?path=` → `{entries: [{name, type, size, mtime}]}`
//! - `GET /servers/:id/files/download?path=` → file bytes + attachment
//! - `POST /servers/:id/files/upload?path=` → multipart, `201 {files: [names]}`
//! - `POST /servers/:id/files/directory {path}` → `201`
//! - `POST /servers/:id/files/rename {from, to}` → `200`
//! - `DELETE /servers/:id/files?path=` → `204`
//! - `POST /servers/:id/files/import?path=` → multipart tar.gz, `200 {files: N}`
//! - `GET /servers/:id/files/export?path=` → tar.gz download
//!
//! Uploads use `axum::extract::Multipart`: each field streams to a tempfile
//! with a running byte cap (`413 TOO_LARGE` past it), then atomically
//! renames into place — whole uploads are never buffered. All sandboxing,
//! listing, atomic writes and archive import/export delegate to
//! `crate::file_service`.

// The controller wires these handlers into `main.rs` after all Phase 4 tasks
// land; until then they are reachable only from tests, which reads as
// dead code to the binary build.

use std::path::{Path, PathBuf};

use axum::{
    extract::{Multipart, Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{
    audit,
    auth::AuthUser,
    file_service::{self, FileError},
    AppState,
};

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

fn file_error(e: FileError) -> Response {
    let status = match &e {
        FileError::NotFound => StatusCode::NOT_FOUND,
        FileError::OutsideRoot => StatusCode::BAD_REQUEST,
        FileError::TooLarge | FileError::TooManyEntries => StatusCode::PAYLOAD_TOO_LARGE,
        FileError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, e.code(), &e.to_string())
}

// ---------------------------------------------------------------------------
// Server row + busy guard (local re-implementations; duplication with
// `routes_servers` is intentional — never edit that module from here)
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct ServerRow {
    id: String,
    data_path: String,
    desired_state: String,
}

async fn fetch_server_row(pool: &SqlitePool, id: &str) -> Result<Option<ServerRow>, sqlx::Error> {
    sqlx::query_as::<_, ServerRow>(
        "SELECT id, data_path, desired_state
         FROM servers WHERE id = ? AND deleted_at IS NULL",
    )
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

// `Response` is axum's standard error vehicle; boxing it buys nothing.
#[allow(clippy::result_large_err)]
async fn ensure_not_busy(pool: &SqlitePool, server_id: &str) -> Result<(), Response> {
    if conflicting_task(pool, server_id).await {
        return Err(error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "a task is already queued/running for this server",
        ));
    }
    Ok(())
}

/// Load the server row and ensure its on-disk root exists.
// `Response` is axum's standard error vehicle; boxing it buys nothing.
#[allow(clippy::result_large_err)]
async fn server_dir(state: &AppState, id: &str) -> Result<(ServerRow, PathBuf), Response> {
    let row = fetch_server_row(&state.pool, id)
        .await
        .map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load server",
            )
        })?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server"))?;
    let root = if row.data_path.is_empty() {
        state.server_root.join(&row.id)
    } else {
        PathBuf::from(&row.data_path)
    };
    std::fs::create_dir_all(&root).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "FILE_ERROR",
            "failed to open server data dir",
        )
    })?;
    Ok((row, root))
}

/// A multipart basename is safe to create inside an already-resolved
/// directory: no separators, never `.` / `..`.
fn sanitize_file_name(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.contains('/') || name.contains('\\') {
        return None;
    }
    Some(name.to_string())
}

// ---------------------------------------------------------------------------
// Listing DTO (`file_service::Entry` keeps the Rust field name `entry_type`;
// the frozen UI contract needs the JSON key `type`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct EntryDto {
    name: String,
    #[serde(rename = "type")]
    entry_type: String,
    size: u64,
    mtime: String,
}

impl From<file_service::Entry> for EntryDto {
    fn from(e: file_service::Entry) -> Self {
        Self {
            name: e.name,
            entry_type: e.entry_type,
            size: e.size,
            mtime: e.mtime,
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming multipart staging (no whole-upload buffering)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct StagedFile {
    file_name: String,
    temp_path: PathBuf,
    size: u64,
}

/// Tempfiles staged by [`stage_upload_parts`]; deleting them on drop keeps
/// failed or over-limit uploads from littering the server data dir.
struct TempGuard {
    paths: Vec<PathBuf>,
}

impl TempGuard {
    fn disarm(&mut self) {
        self.paths.clear();
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
        }
    }
}

// `Response` is axum's standard error vehicle; boxing it buys nothing.
#[allow(clippy::result_large_err)]
fn account(total: &mut u64, n: u64, limit: u64) -> Result<(), Response> {
    *total = total.checked_add(n).unwrap_or(u64::MAX);
    if *total > limit {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "TOO_LARGE",
            "upload exceeds the size limit",
        ));
    }
    Ok(())
}

/// Stream every part of a multipart body; file parts land in tempfiles inside
/// `dest_dir`. The running byte total across all part bodies is capped at
/// `limit` (`413 TOO_LARGE` past it). Text/control fields are drained (and
/// counted) but not stored.
// `Response` is axum's standard error vehicle; boxing it buys nothing.
#[allow(clippy::result_large_err)]
async fn stage_upload_parts(
    mut multipart: Multipart,
    dest_dir: &Path,
    limit: u64,
) -> Result<Vec<StagedFile>, Response> {
    use tokio::io::AsyncWriteExt as _;

    let bad = || {
        error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "invalid multipart body",
        )
    };
    let stage_failed = || {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "FILE_ERROR",
            "failed to stage upload",
        )
    };

    let mut guard = TempGuard { paths: Vec::new() };
    let mut files = Vec::new();
    let mut total: u64 = 0;
    while let Some(mut field) = multipart.next_field().await.map_err(|_| bad())? {
        match field.file_name().map(str::to_owned) {
            None => {
                while let Some(chunk) = field.chunk().await.map_err(|_| bad())? {
                    account(&mut total, chunk.len() as u64, limit)?;
                }
            }
            Some(raw) => {
                let safe = sanitize_file_name(&raw).ok_or_else(|| {
                    error(
                        StatusCode::BAD_REQUEST,
                        "FILE_ERROR",
                        "unsafe file name in upload",
                    )
                })?;
                let temp_path = dest_dir.join(format!(
                    ".upload-{}-{}.part",
                    uuid::Uuid::new_v4(),
                    files.len()
                ));
                let mut sink = tokio::fs::File::create(&temp_path)
                    .await
                    .map_err(|_| stage_failed())?;
                guard.paths.push(temp_path.clone());
                let staged: Result<u64, Response> = async {
                    let mut size = 0u64;
                    while let Some(chunk) = field.chunk().await.map_err(|_| bad())? {
                        account(&mut total, chunk.len() as u64, limit)?;
                        sink.write_all(&chunk).await.map_err(|_| stage_failed())?;
                        size += chunk.len() as u64;
                    }
                    sink.sync_all().await.map_err(|_| stage_failed())?;
                    Ok(size)
                }
                .await;
                // Close before any later rename; the guard still owns cleanup
                // until the caller renames or deletes the tempfile.
                drop(sink);
                let size = staged?;
                files.push(StagedFile {
                    file_name: safe,
                    temp_path,
                    size,
                });
            }
        }
    }
    guard.disarm();
    Ok(files)
}

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DirQuery {
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateDirRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    pub from: String,
    pub to: String,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/servers/:id/files?path= — list one directory.
pub async fn list_files(
    State(state): State<AppState>,
    _user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DirQuery>,
) -> Response {
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let full = match file_service::resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if !full.is_dir() {
        return error(StatusCode::BAD_REQUEST, "FILE_ERROR", "not a directory");
    }
    match file_service::list_dir(&root, &query.path).await {
        Ok(entries) => {
            let entries: Vec<EntryDto> = entries.into_iter().map(EntryDto::from).collect();
            Json(serde_json::json!({ "entries": entries })).into_response()
        }
        Err(e) => file_error(e),
    }
}

/// GET /api/v1/servers/:id/files/download?path= — download one file.
pub async fn download_file(
    State(state): State<AppState>,
    _user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DirQuery>,
) -> Response {
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if query.path.is_empty() {
        return error(StatusCode::BAD_REQUEST, "FILE_ERROR", "no file selected");
    }
    // `resolve` rejects escapes, including symlinks that point outside.
    let full = match file_service::resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    let is_dir = match std::fs::symlink_metadata(&full) {
        Ok(md) if md.file_type().is_symlink() => std::fs::metadata(&full)
            .map(|m| m.is_dir())
            .unwrap_or(false),
        Ok(md) => md.is_dir(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return file_error(FileError::NotFound);
        }
        Err(e) => return file_error(FileError::Io(e.to_string())),
    };
    if is_dir {
        return error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "cannot download a directory",
        );
    }
    let bytes =
        match file_service::read_file(&root, &query.path, file_service::MAX_FILE_BYTES).await {
            Ok(b) => b,
            Err(e) => return file_error(e),
        };
    let filename = full
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', "_"));
    (
        [
            ("content-type", "application/octet-stream".to_string()),
            ("content-disposition", disposition),
        ],
        bytes,
    )
        .into_response()
}

/// POST /api/v1/servers/:id/files/upload?path= — multipart upload into one
/// directory. Streams each field to a tempfile (1 GiB running cap), then
/// atomically renames into place. `201 {files: [names]}`.
pub async fn upload_files(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DirQuery>,
    multipart: Multipart,
) -> Response {
    let request_id = audit::request_id();
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(resp) = ensure_not_busy(&state.pool, &id).await {
        return resp;
    }
    let dest = match file_service::resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if !dest.is_dir() {
        return error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "upload target is not a directory",
        );
    }
    let staged = match stage_upload_parts(multipart, &dest, file_service::MAX_UPLOAD_BYTES).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if staged.is_empty() {
        return error(StatusCode::BAD_REQUEST, "FILE_ERROR", "no files uploaded");
    }
    // Atomic rename into place; best-effort cleanup of staged files that
    // never made it on failure.
    let mut names = Vec::with_capacity(staged.len());
    for part in &staged {
        let final_path = dest.join(&part.file_name);
        if let Err(e) = tokio::fs::rename(&part.temp_path, &final_path).await {
            for rest in &staged {
                let _ = std::fs::remove_file(&rest.temp_path);
            }
            let _ = std::fs::remove_file(&final_path);
            return file_error(FileError::Io(e.to_string()));
        }
        names.push(part.file_name.clone());
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "server.files.write",
        "server",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "op": "upload", "path": query.path, "files": names }),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "files": names })),
    )
        .into_response()
}

/// POST /api/v1/servers/:id/files/directory {path} — create a directory.
pub async fn create_directory(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<CreateDirRequest>,
) -> Response {
    let request_id = audit::request_id();
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(e) = file_service::create_dir(&root, &body.path).await {
        return file_error(e);
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "server.files.write",
        "server",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "op": "mkdir", "path": body.path }),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "path": body.path })),
    )
        .into_response()
}

/// POST /api/v1/servers/:id/files/rename {from, to} — rename/move one entry.
pub async fn rename_entry(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<RenameRequest>,
) -> Response {
    let request_id = audit::request_id();
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(resp) = ensure_not_busy(&state.pool, &id).await {
        return resp;
    }
    if body.from.is_empty() || body.to.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "from and to are required",
        );
    }
    if let Err(e) = file_service::rename_path(&root, &body.from, &body.to).await {
        return file_error(e);
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "server.files.write",
        "server",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "op": "rename", "from": body.from, "to": body.to }),
    )
    .await;
    Json(serde_json::json!({ "from": body.from, "to": body.to })).into_response()
}

/// DELETE /api/v1/servers/:id/files?path= — delete one file or directory
/// tree. Refuses the server root itself. `204`.
pub async fn delete_entry(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DirQuery>,
) -> Response {
    let request_id = audit::request_id();
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(resp) = ensure_not_busy(&state.pool, &id).await {
        return resp;
    }
    if query.path.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "refusing to delete the server root",
        );
    }
    if let Err(e) = file_service::remove_path(&root, &query.path).await {
        return file_error(e);
    }
    audit::record(
        &state.pool,
        Some(&user.id),
        "server.files.write",
        "server",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "op": "delete", "path": query.path }),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

/// POST /api/v1/servers/:id/files/import?path= — extract an uploaded tar.gz
/// into one directory. Requires `desired_state == 'stopped'` (`409
/// SERVER_BUSY` otherwise). `200 {files: N}`.
pub async fn import_archive(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DirQuery>,
    multipart: Multipart,
) -> Response {
    let request_id = audit::request_id();
    let (row, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if row.desired_state != "stopped" {
        return error(
            StatusCode::CONFLICT,
            "SERVER_BUSY",
            "stop the server before importing files",
        );
    }
    if let Err(resp) = ensure_not_busy(&state.pool, &id).await {
        return resp;
    }
    let dest_full = match file_service::resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if dest_full.exists() && !dest_full.is_dir() {
        return error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "import target is not a directory",
        );
    }
    // The destination may be a never-created subdir (resolve already gated
    // it inside the root, so no new traversal surface): create it before
    // staging tempfiles into it.
    if let Err(e) = std::fs::create_dir_all(&dest_full) {
        return file_error(FileError::Io(e.to_string()));
    }
    let staged =
        match stage_upload_parts(multipart, &dest_full, file_service::MAX_UPLOAD_BYTES).await {
            Ok(v) => v,
            Err(resp) => return resp,
        };
    let Some(archive) = staged.first() else {
        return error(StatusCode::BAD_REQUEST, "FILE_ERROR", "no archive uploaded");
    };
    if archive.size == 0 {
        let _ = std::fs::remove_file(&archive.temp_path);
        return error(StatusCode::BAD_REQUEST, "FILE_ERROR", "empty archive");
    }
    // Stream the staged archive through the decoder: the upload is already on
    // disk and must not be copied into memory again.
    let archive_path = archive.temp_path.clone();
    let dest = query.path.clone();
    let extracted = tokio::task::spawn_blocking(move || {
        let dest_full = file_service::resolve(&root, &dest)?;
        let file = std::fs::File::open(&archive_path)
            .map_err(|e| file_service::FileError::Io(e.to_string()))?;
        let reader = flate2::read::GzDecoder::new(file);
        file_service::import_tar_gz_reader(
            reader,
            &dest_full,
            &file_service::DEFAULT_ARCHIVE_LIMITS,
        )
    })
    .await;
    for extra in &staged {
        let _ = std::fs::remove_file(&extra.temp_path);
    }
    let count = match extracted {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return file_error(e),
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "FILE_ERROR",
                "import task failed",
            );
        }
    };
    audit::record(
        &state.pool,
        Some(&user.id),
        "server.files.write",
        "server",
        &id,
        "ok",
        &request_id,
        serde_json::json!({ "op": "import", "path": query.path, "files": count }),
    )
    .await;
    Json(serde_json::json!({ "files": count })).into_response()
}

/// GET /api/v1/servers/:id/files/export?path= — download a subtree as tar.gz.
pub async fn export_archive(
    State(state): State<AppState>,
    _user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DirQuery>,
) -> Response {
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let rel_path = query.path.clone();
    // Build the archive in a private temp file, then stream it off disk. The
    // archive is never buffered in memory, however large the subtree is.
    let tmp = std::env::temp_dir().join(format!("pm-export-{}.tar.gz", uuid::Uuid::new_v4()));
    let write_tmp = tmp.clone();
    let write_root = root.clone();
    let written = tokio::task::spawn_blocking(move || -> Result<(), FileError> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let file = opts
            .open(&write_tmp)
            .map_err(|e| FileError::Io(e.to_string()))?;
        let mut writer = std::io::BufWriter::new(file);
        file_service::export_tar_gz_to(&write_root, &[rel_path.as_str()], &mut writer)?;
        use std::io::Write as _;
        writer.flush().map_err(|e| FileError::Io(e.to_string()))?;
        writer
            .get_ref()
            .sync_all()
            .map_err(|e| FileError::Io(e.to_string()))?;
        Ok(())
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = std::fs::remove_file(&tmp);
            return file_error(e);
        }
        Err(_) => {
            let _ = std::fs::remove_file(&tmp);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "FILE_ERROR",
                "export task failed",
            );
        }
    }

    struct TempExport(std::path::PathBuf);
    impl Drop for TempExport {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    let file = match tokio::fs::File::open(&tmp).await {
        Ok(f) => f,
        Err(_) => {
            let _ = std::fs::remove_file(&tmp);
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "FILE_ERROR",
                "export archive is unavailable",
            );
        }
    };
    let stream =
        futures_util::stream::try_unfold((file, TempExport(tmp)), |(mut file, guard)| async move {
            use tokio::io::AsyncReadExt as _;
            let mut buf = vec![0u8; 64 * 1024];
            let n = file.read(&mut buf).await?;
            if n == 0 {
                Ok::<_, std::io::Error>(None)
            } else {
                buf.truncate(n);
                Ok(Some((axum::body::Bytes::from(buf), (file, guard))))
            }
        });
    (
        [
            ("content-type", "application/gzip".to_string()),
            (
                "content-disposition",
                "attachment; filename=\"files-export.tar.gz\"".to_string(),
            ),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;
    use serde_json::{json, Value};

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for file in [
            include_str!("../../../migrations/0001_init.sql"),
            include_str!("../../../migrations/0010_port_reservations.sql"),
            include_str!("../../../migrations/0002_sessions_audit.sql"),
            include_str!("../../../migrations/0007_update.sql"),
            include_str!("../../../migrations/0009_app_settings.sql"),
        ] {
            for stmt in file.split(";\n") {
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                if !stmt.to_uppercase().contains("CREATE")
                    && !stmt.to_uppercase().contains("INDEX")
                    && !stmt.to_uppercase().contains("ALTER")
                {
                    continue;
                }
                sqlx::query(stmt).execute(&pool).await.unwrap();
            }
        }
        pool
    }

    /// Test app: the full production router (file routes are wired in
    /// `build_router` by the controller with the same permission gates).
    fn files_routes(state: AppState) -> axum::Router {
        crate::build_router(state)
    }

    async fn test_server() -> (TestServer, SqlitePool) {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pm-files-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState::for_tests(pool.clone(), root);
        let app = files_routes(state);
        (TestServer::new(app).unwrap(), pool)
    }

    async fn owner_token(server: &TestServer) -> String {
        let res = server
            .post("/api/v1/auth/setup")
            .json(&json!({"username": "owner", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200);
        crate::auth::session_token_from_set_cookie(res.headers())
            .expect("pm_session cookie after setup")
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

    fn upload_form(name: &str, bytes: Vec<u8>) -> axum_test::multipart::MultipartForm {
        use axum_test::multipart::{MultipartForm, Part};
        MultipartForm::new().add_part(
            "file",
            Part::bytes(bytes)
                .file_name(name.to_string())
                .mime_type("application/octet-stream"),
        )
    }

    fn make_tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut tar_data = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_data);
            for (name, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, *content).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&tar_data).unwrap();
        encoder.finish().unwrap()
    }

    fn list_tar_gz(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        use std::io::Read as _;
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut archive = tar::Archive::new(decoder);
        let mut out = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            out.push((name, content));
        }
        out
    }

    #[tokio::test]
    async fn resolve_existing_file_has_no_trailing_separator() {
        // Regression pin: a resolved existing file must be directly usable
        // for metadata (no trailing separator, which fails with ENOTDIR).
        let root = std::env::temp_dir().join(format!("pm-resolve-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.txt"), b"x").unwrap();
        let p = file_service::resolve(&root, "sub/a.txt").unwrap();
        assert!(
            !p.to_string_lossy().ends_with('/'),
            "trailing separator: {p:?}"
        );
        assert!(p.is_file());
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn upload_download_round_trip() {
        let (server, _) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files1", 25565).await;

        let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        let res = server
            .post(&format!("/api/v1/servers/{id}/files/upload?path="))
            .authorization_bearer(&t)
            .multipart(upload_form("big.bin", payload.clone()))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        assert_eq!(res.json::<Value>()["files"], json!(["big.bin"]));

        let list = server
            .get(&format!("/api/v1/servers/{id}/files?path="))
            .authorization_bearer(&t)
            .await;
        assert_eq!(list.status_code(), 200, "body: {}", list.text());
        let entries = list.json::<Value>()["entries"].as_array().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], json!("big.bin"));
        assert_eq!(entries[0]["type"], json!("file"));
        assert_eq!(entries[0]["size"], json!(1024 * 1024));

        let dl = server
            .get(&format!("/api/v1/servers/{id}/files/download?path=big.bin"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(dl.status_code(), 200, "body: {}", dl.text());
        assert_eq!(dl.as_bytes().as_ref(), payload.as_slice());
        let disposition = dl
            .headers()
            .get("content-disposition")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(disposition.contains("attachment"), "{disposition}");

        // Directories cannot be downloaded.
        let dir_dl = server
            .get(&format!("/api/v1/servers/{id}/files/download?path="))
            .authorization_bearer(&t)
            .await;
        assert_eq!(dir_dl.status_code(), 400, "body: {}", dir_dl.text());
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let (server, _) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files2", 25565).await;

        for probe in ["../x", "a/../../x", "/abs", ".."] {
            let res = server
                .get(&format!("/api/v1/servers/{id}/files?path={probe}"))
                .authorization_bearer(&t)
                .await;
            assert_eq!(res.status_code(), 400, "probe {probe}: {}", res.text());
            assert_eq!(
                res.json::<Value>()["error_code"],
                json!("PATH_OUTSIDE_SERVER_ROOT"),
                "probe {probe}"
            );
        }
    }

    #[tokio::test]
    async fn oversize_rejected() {
        use axum::extract::{FromRequest, Multipart};
        use futures_util::stream;

        // Drive the real `Multipart` extractor with a body split into 7-byte
        // chunks (genuinely incremental parsing), then run the same staging
        // helper the upload handler uses. The production cap is 1 GiB and
        // cannot be uploaded in a test, so a tiny cap exercises the identical
        // code path: past the cap the body fails with `413 TOO_LARGE` and no
        // tempfile may be left behind.
        #[allow(clippy::result_large_err)] // test helper mirroring production shape
        async fn run_stage(
            body: Vec<u8>,
            limit: u64,
            dir: PathBuf,
        ) -> Result<Vec<StagedFile>, Response> {
            let chunks = body
                .chunks(7)
                .map(|c| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(c)))
                .collect::<Vec<_>>();
            let req = axum::extract::Request::builder()
                .header("content-type", "multipart/form-data; boundary=BND")
                .body(axum::body::Body::from_stream(stream::iter(chunks)))
                .unwrap();
            let multipart = Multipart::from_request(req, &())
                .await
                .expect("test body must parse");
            stage_upload_parts(multipart, &dir, limit).await
        }

        let dir = std::env::temp_dir().join(format!("pm-oversize-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let big = [
            b"--BND\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.bin\"\r\n\r\n"
                .as_slice(),
            &[0xABu8; 1024],
            b"\r\n--BND--\r\n".as_slice(),
        ]
        .concat();
        let resp = run_stage(big, 16, dir.clone())
            .await
            .expect_err("upload past the cap must fail");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body_json["error_code"], json!("TOO_LARGE"));
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(leftovers.is_empty(), "tempfiles must be cleaned up");

        // And a body within the cap succeeds through the same path.
        let small = [
            b"--BND\r\nContent-Disposition: form-data; name=\"f\"; filename=\"ok.txt\"\r\n\r\n"
                .as_slice(),
            b"hello".as_slice(),
            b"\r\n--BND--\r\n".as_slice(),
        ]
        .concat();
        let files = run_stage(small, 16, dir.clone())
            .await
            .expect("within-cap upload must succeed");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name, "ok.txt");
        for f in files {
            std::fs::remove_file(f.temp_path).unwrap();
        }

        // Unsafe multipart filenames are rejected end-to-end.
        let evil = [
            b"--BND\r\nContent-Disposition: form-data; name=\"f\"; filename=\"../evil.txt\"\r\n\r\n"
                .as_slice(),
            b"x".as_slice(),
            b"\r\n--BND--\r\n".as_slice(),
        ]
        .concat();
        let resp = run_stage(evil, 1024 * 1024, dir.clone())
            .await
            .expect_err("unsafe filename must fail");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn import_requires_stopped() {
        let (server, pool) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files3", 25565).await;

        let archive = make_tar_gz(&[("hello.txt", b"hi"), ("sub/nested.txt", b"nested")]);
        let send = |archive: Vec<u8>| {
            let server = &server;
            let t = t.clone();
            let id = id.clone();
            async move {
                server
                    .post(&format!("/api/v1/servers/{id}/files/import?path="))
                    .authorization_bearer(&t)
                    .multipart(upload_form("backup.tar.gz", archive))
                    .await
            }
        };

        sqlx::query("UPDATE servers SET desired_state = 'running' WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        let busy = send(archive.clone()).await;
        assert_eq!(busy.status_code(), 409, "body: {}", busy.text());
        assert_eq!(busy.json::<Value>()["error_code"], json!("SERVER_BUSY"));

        sqlx::query("UPDATE servers SET desired_state = 'stopped' WHERE id = ?")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        let ok = send(archive).await;
        assert_eq!(ok.status_code(), 200, "body: {}", ok.text());
        assert_eq!(ok.json::<Value>()["files"], json!(2));

        let dl = server
            .get(&format!(
                "/api/v1/servers/{id}/files/download?path=sub/nested.txt"
            ))
            .authorization_bearer(&t)
            .await;
        assert_eq!(dl.status_code(), 200, "body: {}", dl.text());
        assert_eq!(dl.as_bytes().as_ref(), b"nested");
    }

    #[tokio::test]
    async fn export_downloads_archive() {
        let (server, _) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files4", 25565).await;

        let payload = b"export me".to_vec();
        let up = server
            .post(&format!("/api/v1/servers/{id}/files/upload?path="))
            .authorization_bearer(&t)
            .multipart(upload_form("note.txt", payload.clone()))
            .await;
        assert_eq!(up.status_code(), 201, "body: {}", up.text());

        let res = server
            .get(&format!("/api/v1/servers/{id}/files/export?path="))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let content_type = res
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(content_type.contains("application/gzip"), "{content_type}");
        let bytes = res.as_bytes().to_vec();
        assert!(!bytes.is_empty());

        // The archive re-lists: it decodes as tar.gz and holds our file.
        let entries = list_tar_gz(&bytes);
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.iter().any(|n| n.ends_with("note.txt")), "{names:?}");
        let content = entries
            .iter()
            .find(|(n, _)| n.ends_with("note.txt"))
            .unwrap()
            .1
            .clone();
        assert_eq!(content, payload);
    }

    #[tokio::test]
    async fn mkdir_rename_delete_and_busy_guard() {
        let (server, pool) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files5", 25565).await;

        let mkdir = server
            .post(&format!("/api/v1/servers/{id}/files/directory"))
            .authorization_bearer(&t)
            .json(&json!({"path": "sub"}))
            .await;
        assert_eq!(mkdir.status_code(), 201, "body: {}", mkdir.text());

        let up = server
            .post(&format!("/api/v1/servers/{id}/files/upload?path=sub"))
            .authorization_bearer(&t)
            .multipart(upload_form("a.txt", b"aaa".to_vec()))
            .await;
        assert_eq!(up.status_code(), 201, "body: {}", up.text());

        let ren = server
            .post(&format!("/api/v1/servers/{id}/files/rename"))
            .authorization_bearer(&t)
            .json(&json!({"from": "sub/a.txt", "to": "sub/b.txt"}))
            .await;
        assert_eq!(ren.status_code(), 200, "body: {}", ren.text());

        let list = server
            .get(&format!("/api/v1/servers/{id}/files?path=sub"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(list.status_code(), 200);
        assert_eq!(
            list.json::<Value>()["entries"],
            json!([{
                "name": "b.txt", "type": "file", "size": 3,
                "mtime": list.json::<Value>()["entries"][0]["mtime"],
            }])
        );

        // Refusing the root itself.
        let del_root = server
            .delete(&format!("/api/v1/servers/{id}/files?path="))
            .authorization_bearer(&t)
            .await;
        assert_eq!(del_root.status_code(), 400, "body: {}", del_root.text());

        let del = server
            .delete(&format!("/api/v1/servers/{id}/files?path=sub/b.txt"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(del.status_code(), 204, "body: {}", del.text());

        // A queued task busy-guards upload/rename/delete/import alike.
        sqlx::query(
            "INSERT INTO tasks (id, type, server_id, state) VALUES (?, 'stop', ?, 'queued')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
        let busy_up = server
            .post(&format!("/api/v1/servers/{id}/files/upload?path="))
            .authorization_bearer(&t)
            .multipart(upload_form("c.txt", b"c".to_vec()))
            .await;
        assert_eq!(busy_up.status_code(), 409, "body: {}", busy_up.text());
        assert_eq!(busy_up.json::<Value>()["error_code"], json!("SERVER_BUSY"));
        let busy_del = server
            .delete(&format!("/api/v1/servers/{id}/files?path=sub"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(busy_del.status_code(), 409);
        let busy_ren = server
            .post(&format!("/api/v1/servers/{id}/files/rename"))
            .authorization_bearer(&t)
            .json(&json!({"from": "sub", "to": "sub2"}))
            .await;
        assert_eq!(busy_ren.status_code(), 409);
    }

    #[tokio::test]
    async fn directory_entry_lists_as_dir() {
        // Pins the frozen server contract (`file_service::Entry.entry_type`):
        // directories list as `type: "dir"`, files as `type: "file"`.
        let (server, _) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files-dtype", 25565).await;

        let mkdir = server
            .post(&format!("/api/v1/servers/{id}/files/directory"))
            .authorization_bearer(&t)
            .json(&json!({"path": "sub"}))
            .await;
        assert_eq!(mkdir.status_code(), 201, "body: {}", mkdir.text());
        let up = server
            .post(&format!("/api/v1/servers/{id}/files/upload?path="))
            .authorization_bearer(&t)
            .multipart(upload_form("top.txt", b"top".to_vec()))
            .await;
        assert_eq!(up.status_code(), 201, "body: {}", up.text());

        let list = server
            .get(&format!("/api/v1/servers/{id}/files?path="))
            .authorization_bearer(&t)
            .await;
        assert_eq!(list.status_code(), 200, "body: {}", list.text());
        let entries = list.json::<Value>()["entries"].as_array().unwrap().clone();
        assert_eq!(entries.len(), 2, "entries: {entries:?}");
        let dir = entries.iter().find(|e| e["name"] == json!("sub")).unwrap();
        assert_eq!(dir["type"], json!("dir"), "dir entry: {dir}");
        let file = entries
            .iter()
            .find(|e| e["name"] == json!("top.txt"))
            .unwrap();
        assert_eq!(file["type"], json!("file"), "file entry: {file}");
    }

    #[tokio::test]
    async fn import_into_new_subdir_succeeds() {
        // Regression pin for C3: importing into a never-created subdir must
        // 200 (destination created before staging), not 500.
        let (server, _) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files-newdir", 25565).await;

        let archive = make_tar_gz(&[("hello.txt", b"hi")]);
        let res = server
            .post(&format!(
                "/api/v1/servers/{id}/files/import?path=new/sub/dir"
            ))
            .authorization_bearer(&t)
            .multipart(upload_form("backup.tar.gz", archive))
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        assert_eq!(res.json::<Value>()["files"], json!(1));

        let dl = server
            .get(&format!(
                "/api/v1/servers/{id}/files/download?path=new/sub/dir/hello.txt"
            ))
            .authorization_bearer(&t)
            .await;
        assert_eq!(dl.status_code(), 200, "body: {}", dl.text());
        assert_eq!(dl.as_bytes().as_ref(), b"hi");
    }

    #[tokio::test]
    async fn upload_8mib_route_succeeds() {
        // Route-level oversize evidence (I2): 8 MiB exceeds the 5 MiB default
        // body cap but must succeed end-to-end through the upload route,
        // proving the per-route 1 GiB bypass + streaming staging. The
        // running-cap 413 itself stays pinned at unit level by
        // `oversize_rejected`.
        let (server, _) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files-big", 25565).await;

        let payload: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let res = server
            .post(&format!("/api/v1/servers/{id}/files/upload?path="))
            .authorization_bearer(&t)
            .multipart(upload_form("big8.bin", payload.clone()))
            .await;
        assert_eq!(res.status_code(), 201, "body: {}", res.text());
        assert_eq!(res.json::<Value>()["files"], json!(["big8.bin"]));

        let list = server
            .get(&format!("/api/v1/servers/{id}/files?path="))
            .authorization_bearer(&t)
            .await;
        assert_eq!(list.status_code(), 200, "body: {}", list.text());
        assert_eq!(
            list.json::<Value>()["entries"][0]["size"],
            json!(8 * 1024 * 1024)
        );

        let dl = server
            .get(&format!(
                "/api/v1/servers/{id}/files/download?path=big8.bin"
            ))
            .authorization_bearer(&t)
            .await;
        assert_eq!(dl.status_code(), 200, "body: {}", dl.text());
        assert_eq!(dl.as_bytes().as_ref(), payload.as_slice());
    }

    #[tokio::test]
    async fn malicious_archive_rejected_before_write() {
        // The `tar` crate refuses to *build* archives with `..`/absolute
        // names, so craft the malicious tar byte-by-byte (raw 512-byte
        // headers + gzipped): a valid entry first, then a `..` escape, an
        // absolute path, and a symlink pointing outside the root.
        fn raw_entry(name: &str, kind: u8, link: &str, data: &[u8]) -> Vec<u8> {
            assert!(name.len() < 100 && link.len() < 100);
            let mut hdr = [0u8; 512];
            hdr[..name.len()].copy_from_slice(name.as_bytes());
            hdr[100..108].copy_from_slice(b"0000644\0");
            hdr[108..116].copy_from_slice(b"0000000\0");
            hdr[116..124].copy_from_slice(b"0000000\0");
            let size = format!("{0:011o}\0", data.len());
            hdr[124..124 + size.len()].copy_from_slice(size.as_bytes());
            hdr[156] = kind;
            hdr[157..157 + link.len()].copy_from_slice(link.as_bytes());
            hdr[257..262].copy_from_slice(b"ustar");
            hdr[263..265].copy_from_slice(b"00");
            hdr[148..156].copy_from_slice(b"        ");
            let sum: u32 = hdr.iter().map(|b| *b as u32).sum();
            let chksum = format!("{sum:06o}\0 ");
            hdr[148..156].copy_from_slice(chksum.as_bytes());
            let mut out = hdr.to_vec();
            out.extend_from_slice(data);
            while out.len() % 512 != 0 {
                out.push(0);
            }
            out
        }
        let mut tar_data = Vec::new();
        tar_data.extend(raw_entry("good.txt", b'0', "", b"good"));
        tar_data.extend(raw_entry("../evil.txt", b'0', "", b"evil"));
        tar_data.extend(raw_entry("/abs.txt", b'0', "", b"abs"));
        tar_data.extend(raw_entry("link", b'2', "/etc/passwd", b""));
        tar_data.extend([0u8; 1024].to_vec());
        let archive = {
            use std::io::Write as _;
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&tar_data).unwrap();
            encoder.finish().unwrap()
        };

        let (server, _) = test_server().await;
        let t = owner_token(&server).await;
        let id = make_server(&server, &t, "files6", 25565).await;

        let res = server
            .post(&format!("/api/v1/servers/{id}/files/import?path="))
            .authorization_bearer(&t)
            .multipart(upload_form("evil.tar.gz", archive))
            .await;
        assert_eq!(res.status_code(), 400, "body: {}", res.text());
        assert_eq!(
            res.json::<Value>()["error_code"],
            json!("PATH_OUTSIDE_SERVER_ROOT"),
            "malicious input must be 400, never 500"
        );

        // Nothing was written — not even the valid entry before the bad one.
        let list = server
            .get(&format!("/api/v1/servers/{id}/files?path="))
            .authorization_bearer(&t)
            .await;
        assert_eq!(list.status_code(), 200);
        assert_eq!(list.json::<Value>()["entries"], json!([]));
    }
}
