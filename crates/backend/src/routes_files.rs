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
//! Notes on intentional deviations from the task brief:
//! - The brief names `Multipart` as the upload extractor, but the frozen
//!   `Cargo.toml` builds axum without its `multipart` feature (no `multer`
//!   in the lockfile), so `axum::extract::Multipart` does not exist.
//!   Upload/import instead take `Body` + `HeaderMap` and parse
//!   `multipart/form-data` with a small streaming parser below that keeps
//!   the brief's contract: stream each field to a tempfile with a running
//!   byte cap (`413 TOO_LARGE` past it), never buffering whole uploads.
//! - Task 3 (`file_service.rs`) is still a stub at the time of writing, so
//!   the sandbox helpers here (`resolve`, atomic write, safe tar import /
//!   export) are implemented locally with the exact semantics Task 3
//!   specifies (`MAX_FILE_BYTES`, `MAX_UPLOAD_BYTES`, `ArchiveLimits`).
//!   Swapping the call sites to `crate::file_service` once it lands is
//!   mechanical; no handler signatures change.

// The controller wires these handlers into `main.rs` after all Phase 4 tasks
// land; until then they are reachable only from tests, which reads as
// dead code to the binary build.
#![allow(dead_code)]

use std::path::{Component, Path, PathBuf};

use axum::{
    body::Body,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{audit, auth::AuthUser, AppState};

// ---------------------------------------------------------------------------
// Limits (mirror Task 3 semantics verbatim)
// ---------------------------------------------------------------------------

/// Route-level upload cap: streaming byte total across all fields.
pub const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// Single-file download/read cap.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Archive import caps.
pub const MAX_ARCHIVE_FILES: usize = 100_000;
pub const MAX_ARCHIVE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Error type (mirrors the Task 3 `FileError` contract)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum FileError {
    NotFound,
    OutsideRoot,
    TooLarge,
    TooManyEntries,
    Io(String),
}

impl FileError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::OutsideRoot => "PATH_OUTSIDE_SERVER_ROOT",
            Self::NotFound => "NOT_FOUND",
            Self::TooLarge | Self::TooManyEntries => "TOO_LARGE",
            Self::Io(_) => "FILE_ERROR",
        }
    }
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no such file or directory"),
            Self::OutsideRoot => write!(f, "path escapes the server root"),
            Self::TooLarge => write!(f, "file or upload exceeds the size limit"),
            Self::TooManyEntries => write!(f, "archive holds too many entries"),
            Self::Io(msg) => write!(f, "file error: {msg}"),
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

// ---------------------------------------------------------------------------
// Sandbox: resolve user paths inside the server root
// ---------------------------------------------------------------------------

/// Resolve `user_path` (relative, `/`-separated) inside `root`.
///
/// Rejects absolute paths and lexical `..` escapes up front, then
/// canonicalizes the nearest existing ancestor and re-checks the prefix so
/// symlink parents that point outside the root are rejected too.
pub fn resolve(root: &Path, user_path: &str) -> Result<PathBuf, FileError> {
    if user_path.starts_with('/') {
        return Err(FileError::OutsideRoot);
    }
    let mut rel = PathBuf::new();
    for comp in user_path.split('/') {
        match comp {
            "" | "." => {}
            ".." => return Err(FileError::OutsideRoot),
            c => rel.push(c),
        }
    }
    let joined = root.join(&rel);
    // Nearest existing ancestor (the root itself always exists here).
    let mut anchor = joined.as_path();
    while !anchor.exists() {
        anchor = anchor.parent().ok_or(FileError::OutsideRoot)?;
    }
    let canon_anchor = std::fs::canonicalize(anchor).map_err(|e| FileError::Io(e.to_string()))?;
    let canon_root = std::fs::canonicalize(root).map_err(|e| FileError::Io(e.to_string()))?;
    if !canon_anchor.starts_with(&canon_root) {
        return Err(FileError::OutsideRoot);
    }
    let tail = joined
        .strip_prefix(anchor)
        .map_err(|_| FileError::OutsideRoot)?;
    if tail.as_os_str().is_empty() {
        // The path itself exists: return its canonical form directly. (Joining
        // an empty tail would append a trailing separator, which makes file
        // metadata fail with ENOTDIR.)
        return Ok(canon_anchor);
    }
    Ok(canon_anchor.join(tail))
}

/// A basename is safe to create inside an already-resolved directory.
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
// Directory listing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub size: u64,
    pub mtime: String,
}

fn mtime_string(md: &std::fs::Metadata) -> String {
    match md.modified() {
        Ok(t) => chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339(),
        Err(_) => String::new(),
    }
}

fn list_dir(dir: &Path) -> Result<Vec<Entry>, FileError> {
    let read = std::fs::read_dir(dir).map_err(|e| FileError::Io(e.to_string()))?;
    let mut out = Vec::new();
    for item in read {
        let item = item.map_err(|e| FileError::Io(e.to_string()))?;
        let path = item.path();
        let md = std::fs::symlink_metadata(&path).map_err(|e| FileError::Io(e.to_string()))?;
        let entry_type = if md.file_type().is_symlink() {
            "symlink"
        } else if md.is_dir() {
            "dir"
        } else {
            "file"
        }
        .to_string();
        out.push(Entry {
            name: item.file_name().to_string_lossy().into_owned(),
            entry_type,
            size: md.len(),
            mtime: mtime_string(&md),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn read_capped(path: &Path, max: u64) -> Result<Vec<u8>, FileError> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FileError::NotFound
        } else {
            FileError::Io(e.to_string())
        }
    })?;
    let mut out = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = file
            .read(&mut chunk)
            .map_err(|e| FileError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        if out.len() as u64 + n as u64 > max {
            return Err(FileError::TooLarge);
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Streaming multipart receiver (no whole-upload buffering)
// ---------------------------------------------------------------------------

struct ReceivedFile {
    file_name: String,
    temp_path: PathBuf,
    size: u64,
}

impl std::fmt::Debug for ReceivedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceivedFile")
            .field("file_name", &self.file_name)
            .field("size", &self.size)
            .finish()
    }
}

/// Tempfiles staged by [`receive_files`]; deleting them on drop keeps failed
/// or over-limit uploads from littering the server data dir.
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

fn parse_boundary(content_type: Option<&str>) -> Option<String> {
    let ct = content_type?;
    if !ct.starts_with("multipart/form-data") {
        return None;
    }
    for param in ct.split(';').skip(1) {
        let param = param.trim();
        if let Some(value) = param
            .strip_prefix("boundary=")
            .or_else(|| param.strip_prefix("BOUNDARY="))
        {
            let value = value.trim().trim_matches('"');
            if value.is_empty()
                || value.len() > 200
                || !value.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
            {
                return None;
            }
            return Some(value.to_string());
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=(haystack.len() - needle.len())).find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    find_subslice(buf, b"\r\n")
}

struct BodyReader {
    stream: axum::body::BodyDataStream,
    buf: Vec<u8>,
    eof: bool,
}

impl BodyReader {
    // `Response` is axum's standard error vehicle; boxing it buys nothing.
    #[allow(clippy::result_large_err)]
    async fn fill(&mut self) -> Result<(), Response> {
        use futures_util::StreamExt as _;
        if self.eof {
            return Ok(());
        }
        match self.stream.next().await {
            Some(Ok(chunk)) => {
                self.buf.extend_from_slice(&chunk);
                Ok(())
            }
            Some(Err(_)) => Err(error(
                StatusCode::BAD_REQUEST,
                "FILE_ERROR",
                "failed to read request body",
            )),
            None => {
                self.eof = true;
                Ok(())
            }
        }
    }

    /// One CRLF-terminated line (without the CRLF); `None` on clean EOF.
    // `Response` is axum's standard error vehicle; boxing it buys nothing.
    #[allow(clippy::result_large_err)]
    async fn read_line(&mut self) -> Result<Option<Vec<u8>>, Response> {
        loop {
            if let Some(i) = find_crlf(&self.buf) {
                let line = self.buf[..i].to_vec();
                self.buf.drain(..i + 2);
                return Ok(Some(line));
            }
            if self.eof {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(error(
                    StatusCode::BAD_REQUEST,
                    "FILE_ERROR",
                    "truncated multipart body",
                ));
            }
            if self.buf.len() > 65536 {
                return Err(error(
                    StatusCode::BAD_REQUEST,
                    "FILE_ERROR",
                    "multipart header line too large",
                ));
            }
            self.fill().await?;
        }
    }
}

/// Parse `Content-Disposition: form-data; name=..; filename=..`.
fn parse_disposition(value: &str) -> Option<(String, Option<String>)> {
    let mut parts = value.split(';');
    let kind = parts.next()?.trim();
    if !kind.eq_ignore_ascii_case("form-data") {
        return None;
    }
    let mut name: Option<String> = None;
    let mut filename: Option<String> = None;
    for param in parts {
        let (key, val) = param.split_once('=')?;
        let val = val.trim().trim_matches('"').to_string();
        if key.trim().eq_ignore_ascii_case("name") {
            name = Some(val);
        } else if key.trim().eq_ignore_ascii_case("filename") {
            filename = Some(val);
        }
    }
    Some((name?, filename))
}

// `Response` is axum's standard error vehicle; boxing it buys nothing.
#[allow(clippy::result_large_err)]
async fn write_counted(
    sink: &mut Option<tokio::fs::File>,
    data: &[u8],
    total: &mut u64,
    limit: u64,
) -> Result<u64, Response> {
    *total = total.checked_add(data.len() as u64).unwrap_or(u64::MAX);
    if *total > limit {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "TOO_LARGE",
            "upload exceeds the size limit",
        ));
    }
    if let Some(file) = sink {
        if !data.is_empty() {
            use tokio::io::AsyncWriteExt as _;
            file.write_all(data).await.map_err(|_| {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "FILE_ERROR",
                    "failed to stage upload",
                )
            })?;
        }
    }
    Ok(data.len() as u64)
}

/// Stream every part of a `multipart/form-data` body; file parts land in
/// tempfiles inside `dest_dir`. The running byte total across all part
/// bodies is capped at `limit` (`413 TOO_LARGE` past it).
#[allow(clippy::result_large_err)]
async fn receive_files(
    content_type: Option<&str>,
    body: Body,
    dest_dir: &Path,
    limit: u64,
) -> Result<Vec<ReceivedFile>, Response> {
    let bad = || {
        error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "invalid multipart body",
        )
    };
    let boundary = parse_boundary(content_type).ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "expected multipart/form-data",
        )
    })?;
    let first = format!("--{boundary}").into_bytes();
    let needle = format!("\r\n--{boundary}").into_bytes();

    let mut reader = BodyReader {
        stream: body.into_data_stream(),
        buf: Vec::new(),
        eof: false,
    };
    let mut guard = TempGuard { paths: Vec::new() };

    // Preamble: skip everything up to the first boundary line.
    loop {
        if let Some(i) = find_subslice(&reader.buf, &first) {
            reader.buf.drain(..i + first.len());
            break;
        }
        if reader.eof {
            return Err(bad());
        }
        if reader.buf.len() > first.len() + 65536 {
            return Err(bad());
        }
        reader.fill().await?;
    }
    match reader.read_line().await? {
        Some(line) if line.is_empty() => {}
        Some(line) if line.starts_with(b"--") => return Ok(Vec::new()),
        _ => return Err(bad()),
    }

    let mut files = Vec::new();
    let mut total: u64 = 0;
    loop {
        // Part headers.
        let mut field_name: Option<String> = None;
        let mut file_name: Option<String> = None;
        loop {
            let raw = reader.read_line().await?.ok_or_else(bad)?;
            if raw.is_empty() {
                break;
            }
            let text = std::str::from_utf8(&raw).map_err(|_| bad())?;
            let lower = text.to_ascii_lowercase();
            if let Some(value) = lower
                .strip_prefix("content-disposition:")
                .and_then(|_| text.split_once(':').map(|(_, v)| v.trim()))
            {
                let (name, filename) = parse_disposition(value).ok_or_else(bad)?;
                field_name = Some(name);
                file_name = filename;
            }
        }
        if field_name.is_none() {
            return Err(bad());
        }

        // Stage file parts in a tempfile inside the destination dir.
        let mut sink: Option<tokio::fs::File> = None;
        let mut staged: Option<(String, PathBuf)> = None;
        let mut part_size: u64 = 0;
        if let Some(raw_name) = file_name {
            let safe = sanitize_file_name(&raw_name).ok_or_else(|| {
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
            let file = tokio::fs::File::create(&temp_path).await.map_err(|_| {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "FILE_ERROR",
                    "failed to stage upload",
                )
            })?;
            guard.paths.push(temp_path.clone());
            sink = Some(file);
            staged = Some((safe, temp_path));
        }

        // Part body: flush everything that cannot be the delimiter tail.
        let keep = needle.len();
        let mut final_boundary = false;
        loop {
            if let Some(i) = find_subslice(&reader.buf, &needle) {
                let chunk: Vec<u8> = reader.buf[..i].to_vec();
                part_size += write_counted(&mut sink, &chunk, &mut total, limit)
                    .await
                    .inspect_err(|_| {
                        drop(sink.take());
                    })?;
                reader.buf.drain(..i + needle.len());
                let line = reader.read_line().await?.ok_or_else(bad)?;
                if line.starts_with(b"--") {
                    final_boundary = true;
                } else if !line.is_empty() {
                    return Err(bad());
                }
                break;
            }
            if reader.eof {
                return Err(error(
                    StatusCode::BAD_REQUEST,
                    "FILE_ERROR",
                    "truncated multipart body",
                ));
            }
            if reader.buf.len() > keep {
                let n = reader.buf.len() - keep;
                let chunk: Vec<u8> = reader.buf[..n].to_vec();
                part_size += write_counted(&mut sink, &chunk, &mut total, limit)
                    .await
                    .inspect_err(|_| {
                        drop(sink.take());
                    })?;
                reader.buf.drain(..n);
            }
            reader.fill().await?;
        }

        // Flush + close before any later rename; the guard still owns cleanup
        // until the caller renames or deletes the tempfile.
        if let Some(mut file) = sink.take() {
            use tokio::io::AsyncWriteExt as _;
            file.flush().await.map_err(|_| {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "FILE_ERROR",
                    "failed to stage upload",
                )
            })?;
            drop(file);
        }
        if let Some((safe, temp_path)) = staged {
            files.push(ReceivedFile {
                file_name: safe,
                temp_path,
                size: part_size,
            });
        }
        if final_boundary {
            break;
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
    let dir = match resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if !dir.is_dir() {
        return error(StatusCode::BAD_REQUEST, "FILE_ERROR", "not a directory");
    }
    match list_dir(&dir) {
        Ok(entries) => Json(serde_json::json!({ "entries": entries })).into_response(),
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
    let mut target = match resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    // Never serve through a symlink that escapes the root.
    if let Ok(md) = std::fs::symlink_metadata(&target) {
        if md.file_type().is_symlink() {
            let canon_root = match std::fs::canonicalize(&root) {
                Ok(p) => p,
                Err(_) => {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "FILE_ERROR",
                        "failed to read file",
                    );
                }
            };
            match std::fs::canonicalize(&target) {
                Ok(canon) if canon.starts_with(&canon_root) => target = canon,
                _ => return file_error(FileError::OutsideRoot),
            }
        }
    }
    let md = match std::fs::metadata(&target) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return file_error(FileError::NotFound)
        }
        Err(_) => return file_error(FileError::Io("failed to read file".to_string())),
    };
    if md.is_dir() {
        return error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "cannot download a directory",
        );
    }
    let bytes = match read_capped(&target, MAX_FILE_BYTES) {
        Ok(b) => b,
        Err(e) => return file_error(e),
    };
    let filename = target
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
    headers: HeaderMap,
    body: Body,
) -> Response {
    let request_id = audit::request_id();
    let (_, root) = match server_dir(&state, &id).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Err(resp) = ensure_not_busy(&state.pool, &id).await {
        return resp;
    }
    let dest = match resolve(&root, &query.path) {
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
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let staged = match receive_files(content_type, body, &dest, MAX_UPLOAD_BYTES).await {
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
    let dir = match resolve(&root, &body.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return file_error(FileError::Io(e.to_string()));
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
    let from = match resolve(&root, &body.from) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    let to = match resolve(&root, &body.to) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if !from.exists() && std::fs::symlink_metadata(&from).is_err() {
        return file_error(FileError::NotFound);
    }
    if let Some(parent) = to.parent() {
        if !parent.is_dir() {
            return error(
                StatusCode::BAD_REQUEST,
                "FILE_ERROR",
                "rename target directory does not exist",
            );
        }
    }
    if let Err(e) = std::fs::rename(&from, &to) {
        return file_error(FileError::Io(e.to_string()));
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
    let target = match resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    let md = match std::fs::symlink_metadata(&target) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return file_error(FileError::NotFound)
        }
        Err(e) => return file_error(FileError::Io(e.to_string())),
    };
    let removed = if md.is_dir() && !md.file_type().is_symlink() {
        std::fs::remove_dir_all(&target)
    } else {
        std::fs::remove_file(&target)
    };
    if let Err(e) = removed {
        return file_error(FileError::Io(e.to_string()));
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

// ---------------------------------------------------------------------------
// Archive import / export (blocking tar/gzip work in `spawn_blocking`)
// ---------------------------------------------------------------------------

fn check_entry_path(raw: &Path) -> Result<PathBuf, FileError> {
    if raw.as_os_str().is_empty() {
        return Err(FileError::Io("empty tar entry name".to_string()));
    }
    if raw.is_absolute() {
        return Err(FileError::OutsideRoot);
    }
    let mut clean = PathBuf::new();
    for comp in raw.components() {
        match comp {
            Component::Normal(c) => clean.push(c),
            Component::CurDir => {}
            Component::ParentDir => return Err(FileError::OutsideRoot),
            _ => return Err(FileError::OutsideRoot),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(FileError::Io("empty tar entry name".to_string()));
    }
    Ok(clean)
}

/// A symlink/hardlink entry is safe only when its target stays inside the
/// archive root.
fn check_link_target(link_rel: &Path, target: &Path) -> Result<(), FileError> {
    if target.is_absolute() {
        return Err(FileError::OutsideRoot);
    }
    let base_depth = link_rel
        .parent()
        .map(|p| p.components().count())
        .unwrap_or(0) as i64;
    let mut depth = base_depth;
    for comp in target.components() {
        match comp {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(FileError::OutsideRoot);
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir | Component::Prefix(_) | Component::RootDir => {}
        }
    }
    Ok(())
}

#[derive(Debug)]
enum PlannedKind {
    Dir,
    File,
    Link(PathBuf),
}

/// Validate every entry BEFORE any write (counts + sizes + names + link
/// targets), then extract. Returns the number of regular files written.
fn extract_archive(archive: &Path, dest: &Path) -> Result<usize, FileError> {
    use std::io::Read as _;

    // Pass 1: validate (headers only, no writes).
    let mut plans: Vec<(PathBuf, PlannedKind)> = Vec::new();
    let mut total_bytes: u64 = 0;
    {
        let file = std::fs::File::open(archive).map_err(|e| FileError::Io(e.to_string()))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        let entries = tar.entries().map_err(|e| FileError::Io(e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| FileError::Io(e.to_string()))?;
            let raw = entry
                .path()
                .map_err(|e| FileError::Io(e.to_string()))?
                .into_owned();
            let rel = check_entry_path(&raw)?;
            let header = entry.header();
            let kind = if header.entry_type().is_dir() {
                PlannedKind::Dir
            } else if header.entry_type().is_file() {
                total_bytes = total_bytes.saturating_add(entry.size());
                if total_bytes > MAX_ARCHIVE_BYTES {
                    return Err(FileError::TooLarge);
                }
                PlannedKind::File
            } else if header.entry_type().is_symlink() || header.entry_type().is_hard_link() {
                let target = entry
                    .link_name()
                    .map_err(|e| FileError::Io(e.to_string()))?
                    .ok_or_else(|| FileError::Io("link entry without target".to_string()))?
                    .into_owned();
                check_link_target(&rel, &target)?;
                PlannedKind::Link(target)
            } else {
                // Device nodes, fifos, sockets and other special files are
                // never materialised from an uploaded archive.
                return Err(FileError::Io(format!(
                    "unsupported tar entry type at {}",
                    rel.display()
                )));
            };
            plans.push((rel, kind));
            if plans.len() > MAX_ARCHIVE_FILES {
                return Err(FileError::TooManyEntries);
            }
        }
    }

    // Pass 2: extract (same file, same order).
    let mut files_written = 0usize;
    let mut seen_bytes: u64 = 0;
    {
        let file = std::fs::File::open(archive).map_err(|e| FileError::Io(e.to_string()))?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        let entries = tar.entries().map_err(|e| FileError::Io(e.to_string()))?;
        for (entry, (rel, _)) in entries.zip(plans.iter()) {
            let mut entry = entry.map_err(|e| FileError::Io(e.to_string()))?;
            let out = resolve(dest, &rel.to_string_lossy())?;
            match entry.header().entry_type() {
                t if t.is_dir() => {
                    std::fs::create_dir_all(&out).map_err(|e| FileError::Io(e.to_string()))?;
                }
                t if t.is_file() => {
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| FileError::Io(e.to_string()))?;
                    }
                    let mut sink =
                        std::fs::File::create(&out).map_err(|e| FileError::Io(e.to_string()))?;
                    let mut chunk = [0u8; 65536];
                    loop {
                        let n = entry
                            .read(&mut chunk)
                            .map_err(|e| FileError::Io(e.to_string()))?;
                        if n == 0 {
                            break;
                        }
                        seen_bytes = seen_bytes.saturating_add(n as u64);
                        if seen_bytes > MAX_ARCHIVE_BYTES {
                            let _ = std::fs::remove_file(&out);
                            return Err(FileError::TooLarge);
                        }
                        std::io::Write::write_all(&mut sink, &chunk[..n])
                            .map_err(|e| FileError::Io(e.to_string()))?;
                    }
                    files_written += 1;
                }
                t if t.is_symlink() || t.is_hard_link() => {
                    let target = entry
                        .link_name()
                        .map_err(|e| FileError::Io(e.to_string()))?
                        .ok_or_else(|| FileError::Io("link entry without target".to_string()))?
                        .into_owned();
                    if let Some(parent) = out.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| FileError::Io(e.to_string()))?;
                    }
                    let _ = std::fs::remove_file(&out);
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&target, &out)
                        .map_err(|e| FileError::Io(e.to_string()))?;
                    #[cfg(not(unix))]
                    return Err(FileError::Io(format!(
                        "cannot materialise link at {}",
                        target.display()
                    )));
                }
                _ => return Err(FileError::Io("unsupported tar entry type".to_string())),
            }
            let _ = rel;
        }
    }
    Ok(files_written)
}

fn build_export(root: &Path, sub: &Path) -> Result<Vec<u8>, FileError> {
    let canon_root = std::fs::canonicalize(root).map_err(|e| FileError::Io(e.to_string()))?;
    let mut dirs = Vec::new();
    let mut seed: Vec<PathBuf> = vec![sub.to_path_buf()];
    let mut count: usize = 0;
    let mut bytes: u64 = 0;
    let mut out = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);
        // A single file exports as a one-entry archive.
        if sub.is_file()
            && !std::fs::symlink_metadata(sub)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        {
            let rel = sub
                .strip_prefix(&canon_root)
                .map_err(|_| FileError::OutsideRoot)?
                .to_path_buf();
            let mut file = std::fs::File::open(sub).map_err(|e| FileError::Io(e.to_string()))?;
            tar.append_file(&rel, &mut file)
                .map_err(|e| FileError::Io(e.to_string()))?;
        } else {
            dirs.append(&mut seed);
            while let Some(dir) = dirs.pop() {
                let read = std::fs::read_dir(&dir).map_err(|e| FileError::Io(e.to_string()))?;
                for item in read {
                    let item = item.map_err(|e| FileError::Io(e.to_string()))?;
                    let path = item.path();
                    let rel = path
                        .strip_prefix(&canon_root)
                        .map_err(|_| FileError::OutsideRoot)?
                        .to_path_buf();
                    if rel.as_os_str().is_empty() {
                        continue;
                    }
                    let md = std::fs::symlink_metadata(&path)
                        .map_err(|e| FileError::Io(e.to_string()))?;
                    count += 1;
                    if count > MAX_ARCHIVE_FILES {
                        return Err(FileError::TooManyEntries);
                    }
                    if md.file_type().is_symlink() {
                        let target =
                            std::fs::read_link(&path).map_err(|e| FileError::Io(e.to_string()))?;
                        let mut header = tar::Header::new_gnu();
                        header.set_entry_type(tar::EntryType::Symlink);
                        header.set_cksum();
                        tar.append_link(&mut header, &rel, &target)
                            .map_err(|e| FileError::Io(e.to_string()))?;
                    } else if md.is_dir() {
                        tar.append_dir(&rel, &path)
                            .map_err(|e| FileError::Io(e.to_string()))?;
                        dirs.push(path);
                    } else if md.is_file() {
                        bytes = bytes.saturating_add(md.len());
                        if bytes > MAX_ARCHIVE_BYTES {
                            return Err(FileError::TooLarge);
                        }
                        let mut file =
                            std::fs::File::open(&path).map_err(|e| FileError::Io(e.to_string()))?;
                        tar.append_file(&rel, &mut file)
                            .map_err(|e| FileError::Io(e.to_string()))?;
                    }
                }
            }
        }
        let encoder = tar.into_inner().map_err(|e| FileError::Io(e.to_string()))?;
        encoder.finish().map_err(|e| FileError::Io(e.to_string()))?;
    }
    Ok(out)
}

/// POST /api/v1/servers/:id/files/import?path= — extract an uploaded tar.gz
/// into one directory. Requires `desired_state == 'stopped'` (`409
/// SERVER_BUSY` otherwise). `200 {files: N}`.
pub async fn import_archive(
    State(state): State<AppState>,
    user: AuthUser,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<DirQuery>,
    headers: HeaderMap,
    body: Body,
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
    let dest = match resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if let Err(e) = std::fs::create_dir_all(&dest) {
        return file_error(FileError::Io(e.to_string()));
    }
    if !dest.is_dir() {
        return error(
            StatusCode::BAD_REQUEST,
            "FILE_ERROR",
            "import target is not a directory",
        );
    }
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let staged = match receive_files(content_type, body, &dest, MAX_UPLOAD_BYTES).await {
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
    let temp_path = archive.temp_path.clone();
    for extra in staged.iter().skip(1) {
        let _ = std::fs::remove_file(&extra.temp_path);
    }
    let scan_path = temp_path.clone();
    let extracted = tokio::task::spawn_blocking(move || extract_archive(&scan_path, &dest)).await;
    let _ = std::fs::remove_file(&temp_path);
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
    let sub = match resolve(&root, &query.path) {
        Ok(p) => p,
        Err(e) => return file_error(e),
    };
    if !sub.exists() {
        return file_error(FileError::NotFound);
    }
    let bytes = match tokio::task::spawn_blocking(move || build_export(&root, &sub)).await {
        Ok(Ok(b)) => b,
        Ok(Err(e)) => return file_error(e),
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "FILE_ERROR",
                "export task failed",
            );
        }
    };
    (
        [
            ("content-type", "application/gzip".to_string()),
            (
                "content-disposition",
                "attachment; filename=\"files-export.tar.gz\"".to_string(),
            ),
        ],
        bytes,
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

    /// Test-only wiring mirroring the controller's future routes + the
    /// `ServerFilesRead`/`ServerFilesWrite` permission layers from the plan.
    fn files_routes(state: AppState) -> axum::Router {
        use crate::auth::{self, Permission};
        use axum::{
            middleware,
            routing::{delete, get, post},
        };
        let pool = state.pool.clone();
        axum::Router::new()
            .route(
                "/api/v1/servers/:id/files",
                get(list_files).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesRead,
                ))),
            )
            .route(
                "/api/v1/servers/:id/files",
                delete(delete_entry).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesWrite,
                ))),
            )
            .route(
                "/api/v1/servers/:id/files/download",
                get(download_file).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesRead,
                ))),
            )
            .route(
                "/api/v1/servers/:id/files/upload",
                post(upload_files).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesWrite,
                ))),
            )
            .route(
                "/api/v1/servers/:id/files/directory",
                post(create_directory).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesWrite,
                ))),
            )
            .route(
                "/api/v1/servers/:id/files/rename",
                post(rename_entry).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesWrite,
                ))),
            )
            .route(
                "/api/v1/servers/:id/files/import",
                post(import_archive).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesWrite,
                ))),
            )
            .route(
                "/api/v1/servers/:id/files/export",
                get(export_archive).route_layer(middleware::from_fn(auth::require_permission(
                    Permission::ServerFilesRead,
                ))),
            )
            .route_layer(middleware::from_fn_with_state(pool, auth::auth_middleware))
            .with_state(state)
    }

    async fn test_server() -> (TestServer, SqlitePool) {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pm-files-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState::for_tests(pool.clone(), root);
        let app = crate::build_router(state.clone()).merge(files_routes(state));
        (TestServer::new(app).unwrap(), pool)
    }

    async fn owner_token(server: &TestServer) -> String {
        let res = server
            .post("/api/v1/auth/setup")
            .json(&json!({"username": "owner", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200);
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
        // Same streaming receiver the upload handler uses, exercised with a
        // tiny cap (the production cap is 1 GiB and cannot be uploaded in a
        // test). Past the cap the body must fail with 413 TOO_LARGE and no
        // tempfile may be left behind.
        let dir = std::env::temp_dir().join(format!("pm-oversize-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = vec![0xABu8; 1024];
        let body = [
            b"--BND\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n".as_slice(),
            data.as_slice(),
            b"\r\n--BND--\r\n".as_slice(),
        ]
        .concat();
        let result = receive_files(
            Some("multipart/form-data; boundary=BND"),
            Body::from(body),
            &dir,
            16,
        )
        .await;
        let resp = result.expect_err("upload past the cap must fail");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body_json["error_code"], json!("TOO_LARGE"));
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert!(leftovers.is_empty(), "tempfiles must be cleaned up");

        // And a body within the cap succeeds through the same path.
        let small = b"hello".to_vec();
        let body = [
            b"--BND\r\nContent-Disposition: form-data; name=\"f\"; filename=\"ok.txt\"\r\n\r\n".as_slice(),
            small.as_slice(),
            b"\r\n--BND--\r\n".as_slice(),
        ]
        .concat();
        let files = receive_files(
            Some("multipart/form-data; boundary=BND"),
            Body::from(body),
            &dir,
            16,
        )
        .await
        .expect("within-cap upload must succeed");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name, "ok.txt");
        for f in files {
            std::fs::remove_file(f.temp_path).unwrap();
        }
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
        assert!(
            res.status_code() == 400 || res.status_code() == 500,
            "body: {}",
            res.text()
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
