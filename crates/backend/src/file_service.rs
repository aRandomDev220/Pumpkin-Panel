//! Sandboxed file service over a server root directory.
//!
//! Every public operation resolves the caller-supplied path with [`resolve`],
//! which rejects absolute paths and any `..` component lexically and then
//! re-checks the canonicalized-if-exists path against the canonical root, so
//! a symlink parent (or final component) pointing outside the root is
//! rejected with [`FileError::OutsideRoot`].
//!
//! Archive handling is two-phase: [`import_tar_gz`] fully parses and
//! validates every entry (names, device nodes, symlink/hardlink targets,
//! count and byte caps) *before* creating or writing anything, and the write
//! phase refuses to create anything through a pre-existing symlink.
//!
//! The async functions perform only metadata/small I/O on the async runtime.
//! The blocking archive functions ([`export_tar_gz`], [`import_tar_gz`]) are
//! synchronous and CPU/gzip-bound: callers must run them inside
//! `tokio::task::spawn_blocking`.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Single-file read/write cap (64 MiB).
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Route-level upload cap reference (1 GiB).
pub const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;

/// A directory listing entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    pub name: String,
    /// `"file"` | `"dir"` | `"symlink"`.
    pub entry_type: String,
    pub size: u64,
    /// RFC 3339 modification time, or `""` when unavailable.
    pub mtime: String,
}

/// Limits for [`import_tar_gz`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Maximum number of archive entries (files, dirs, links combined).
    pub max_files: usize,
    /// Maximum total bytes of regular-file payload extracted.
    pub max_bytes: u64,
}

/// Default archive limits: 100k entries, 10 GiB total.
pub const DEFAULT_ARCHIVE_LIMITS: ArchiveLimits = ArchiveLimits {
    max_files: 100_000,
    max_bytes: 10 * 1024 * 1024 * 1024,
};

/// Errors from the sandboxed file service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error("not found")]
    NotFound,
    #[error("path escapes server root")]
    OutsideRoot,
    #[error("file too large")]
    TooLarge,
    #[error("too many archive entries")]
    TooManyEntries,
    #[error("file error: {0}")]
    Io(String),
}

impl FileError {
    /// Stable API error code.
    pub fn code(&self) -> &'static str {
        match self {
            FileError::OutsideRoot => "PATH_OUTSIDE_SERVER_ROOT",
            FileError::NotFound => "NOT_FOUND",
            FileError::TooLarge | FileError::TooManyEntries => "TOO_LARGE",
            FileError::Io(_) => "FILE_ERROR",
        }
    }
}

fn map_io(e: std::io::Error) -> FileError {
    if e.kind() == std::io::ErrorKind::NotFound {
        FileError::NotFound
    } else {
        FileError::Io(e.to_string())
    }
}

/// Canonicalize `root` when it exists; otherwise fall back to an absolute
/// lexical form so prefix checks still behave.
fn canonical_root(root: &Path) -> PathBuf {
    if let Ok(c) = root.canonicalize() {
        return c;
    }
    if root.is_absolute() {
        return root.to_path_buf();
    }
    std::env::current_dir()
        .map(|c| c.join(root))
        .unwrap_or_else(|_| root.to_path_buf())
}

/// Resolve a caller-supplied path against `root`.
///
/// * Absolute paths and any `..` component are rejected lexically, even when
///   they would stay inside the root (`a/../b` is rejected too).
/// * The nearest existing ancestor (found with `symlink_metadata`, so
///   symlinks themselves count as existing) is canonicalized and the
///   remaining non-existent tail re-appended; the result must equal or sit
///   under the canonical root. A symlink parent or final component pointing
///   outside therefore yields [`FileError::OutsideRoot`].
/// * `""` and `"."` resolve to the root itself.
pub fn resolve(root: &Path, user_path: &str) -> Result<PathBuf, FileError> {
    if user_path.contains('\0') {
        return Err(FileError::Io("invalid path".to_string()));
    }
    let rel = Path::new(user_path);
    if rel.is_absolute() {
        return Err(FileError::OutsideRoot);
    }
    for comp in rel.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(FileError::OutsideRoot)
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    let joined = root.join(rel);
    let canon_root = canonical_root(root);

    // Nearest existing ancestor (lstat: symlinks count as existing, including
    // dangling ones).
    let mut ancestor: &Path = &joined;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        match ancestor.symlink_metadata() {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match ancestor.parent() {
                Some(p) => {
                    if let Some(f) = ancestor.file_name() {
                        tail.push(f);
                    }
                    if p == ancestor {
                        return Err(FileError::NotFound);
                    }
                    ancestor = p;
                }
                None => return Err(FileError::NotFound),
            },
            Err(e) => return Err(map_io(e)),
        }
    }
    let canon_base = ancestor.canonicalize().map_err(map_io)?;
    let mut full = canon_base;
    for comp in tail.iter().rev() {
        full.push(comp);
    }
    if full != canon_root && !full.starts_with(&canon_root) {
        return Err(FileError::OutsideRoot);
    }
    Ok(full)
}

fn mtime_string(md: &std::fs::Metadata) -> String {
    md.modified()
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .unwrap_or_default()
}

/// List a directory inside the root. `path` of `""`/`"."` lists the root.
pub async fn list_dir(root: &Path, path: &str) -> Result<Vec<Entry>, FileError> {
    let full = resolve(root, path)?;
    let mut rd = tokio::fs::read_dir(&full).await.map_err(map_io)?;
    let mut out = Vec::new();
    while let Some(ent) = rd.next_entry().await.map_err(map_io)? {
        // `file_type` does not follow symlinks.
        let ft = ent.file_type().await.map_err(map_io)?;
        let md = tokio::fs::symlink_metadata(ent.path())
            .await
            .map_err(map_io)?;
        let entry_type = if ft.is_symlink() {
            "symlink"
        } else if ft.is_dir() {
            "dir"
        } else {
            "file"
        }
        .to_string();
        out.push(Entry {
            name: ent.file_name().to_string_lossy().into_owned(),
            entry_type,
            size: if ft.is_dir() { 0 } else { md.len() },
            mtime: mtime_string(&md),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Read a file inside the root, enforcing `max_bytes`.
pub async fn read_file(root: &Path, path: &str, max_bytes: u64) -> Result<Vec<u8>, FileError> {
    let full = resolve(root, path)?;
    // Fast path: reject oversized files without reading them.
    let md = tokio::fs::symlink_metadata(&full).await.map_err(map_io)?;
    if md.is_dir() {
        return Err(FileError::Io("not a file".to_string()));
    }
    if md.len() > max_bytes {
        return Err(FileError::TooLarge);
    }
    let f = tokio::fs::File::open(&full).await.map_err(map_io)?;
    // `take` guards against TOCTOU growth between the metadata check above
    // and the read.
    let mut limited = f.take(max_bytes.saturating_add(1));
    let mut buf = Vec::new();
    limited.read_to_end(&mut buf).await.map_err(map_io)?;
    if buf.len() as u64 > max_bytes {
        return Err(FileError::TooLarge);
    }
    Ok(buf)
}

static TMP_CTR: AtomicU64 = AtomicU64::new(0);

fn tmp_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        ".tmp-{}-{}-{}",
        std::process::id(),
        nanos,
        TMP_CTR.fetch_add(1, Ordering::SeqCst)
    )
}

/// Atomically write `data` to `path`: bytes go to a temp file in the same
/// directory, are fsynced, then renamed over the target. The existing file is
/// untouched unless the rename succeeds. Symlinks are never followed: a final
/// symlink component is refused.
///
/// Currently the upload path stages via its own helper; this remains the
/// canonical small-write primitive for future config/file endpoints.
#[allow(dead_code)]
pub async fn write_file_atomic(
    root: &Path,
    path: &str,
    data: &[u8],
    max_bytes: u64,
) -> Result<(), FileError> {
    if data.len() as u64 > max_bytes {
        return Err(FileError::TooLarge);
    }
    let full = resolve(root, path)?;
    if full == canonical_root(root) {
        return Err(FileError::Io("refusing to write server root".to_string()));
    }
    let parent = full
        .parent()
        .ok_or_else(|| FileError::Io("invalid path".to_string()))?
        .to_path_buf();
    tokio::fs::create_dir_all(&parent).await.map_err(map_io)?;
    if let Ok(md) = tokio::fs::symlink_metadata(&full).await {
        if md.file_type().is_symlink() {
            return Err(FileError::OutsideRoot);
        }
    }
    let tmp = parent.join(tmp_name());
    let result = async {
        let mut f = tokio::fs::File::create(&tmp).await.map_err(map_io)?;
        f.write_all(data).await.map_err(map_io)?;
        f.sync_all().await.map_err(map_io)?;
        drop(f);
        tokio::fs::rename(&tmp, &full).await.map_err(map_io)?;
        Ok::<(), FileError>(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}

/// Create a directory (and parents) inside the root.
pub async fn create_dir(root: &Path, path: &str) -> Result<(), FileError> {
    let full = resolve(root, path)?;
    tokio::fs::create_dir_all(&full).await.map_err(map_io)?;
    Ok(())
}

/// Rename/move a path inside the root.
pub async fn rename_path(root: &Path, from: &str, to: &str) -> Result<(), FileError> {
    let a = resolve(root, from)?;
    let b = resolve(root, to)?;
    let canon = canonical_root(root);
    if a == canon || b == canon {
        return Err(FileError::Io("refusing to rename server root".to_string()));
    }
    if let Some(parent) = b.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(map_io)?;
    }
    tokio::fs::rename(&a, &b).await.map_err(map_io)?;
    Ok(())
}

/// Remove a file, symlink, or directory tree inside the root. Refuses the
/// root itself. Symlinks are removed as links, never followed.
pub async fn remove_path(root: &Path, path: &str) -> Result<(), FileError> {
    let full = resolve(root, path)?;
    if full == canonical_root(root) {
        return Err(FileError::Io("refusing to remove server root".to_string()));
    }
    let md = tokio::fs::symlink_metadata(&full).await.map_err(map_io)?;
    if md.file_type().is_symlink() || !md.is_dir() {
        tokio::fs::remove_file(&full).await.map_err(map_io)?;
    } else {
        tokio::fs::remove_dir_all(&full).await.map_err(map_io)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Archives (blocking: run inside `spawn_blocking`)
// ---------------------------------------------------------------------------

struct ExportItem {
    full: PathBuf,
    arc: PathBuf,
}

fn collect_export(full: &Path, arc: &Path, items: &mut Vec<ExportItem>) -> Result<(), FileError> {
    let md = std::fs::symlink_metadata(full).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            FileError::NotFound
        } else {
            FileError::Io(e.to_string())
        }
    })?;
    let ft = md.file_type();
    if ft.is_dir() && !ft.is_symlink() {
        items.push(ExportItem {
            full: full.to_path_buf(),
            arc: arc.to_path_buf(),
        });
        let mut kids: Vec<_> = std::fs::read_dir(full)
            .map_err(map_io)?
            .collect::<Result<_, _>>()
            .map_err(map_io)?;
        kids.sort_by_key(|e| e.file_name());
        for kid in kids {
            let child_arc = if arc.as_os_str().is_empty() {
                PathBuf::from(kid.file_name())
            } else {
                arc.join(kid.file_name())
            };
            collect_export(&kid.path(), &child_arc, items)?;
        }
    } else if ft.is_symlink() || ft.is_file() {
        items.push(ExportItem {
            full: full.to_path_buf(),
            arc: arc.to_path_buf(),
        });
    }
    // Sockets, fifos, device nodes are silently skipped on export: they can
    // never round-trip through the validating importer.
    Ok(())
}

fn append_item<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    item: &ExportItem,
) -> Result<(), FileError> {
    let md = std::fs::symlink_metadata(&item.full).map_err(map_io)?;
    let ft = md.file_type();
    if ft.is_symlink() {
        let target = std::fs::read_link(&item.full).map_err(map_io)?;
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_path(&item.arc)
            .map_err(|e| FileError::Io(e.to_string()))?;
        h.set_link_name(&target)
            .map_err(|e| FileError::Io(e.to_string()))?;
        h.set_mode(0o777);
        h.set_size(0);
        h.set_cksum();
        builder
            .append(&h, &[][..])
            .map_err(|e| FileError::Io(e.to_string()))?;
    } else if ft.is_dir() {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_path(&item.arc)
            .map_err(|e| FileError::Io(e.to_string()))?;
        h.set_metadata(&md);
        h.set_cksum();
        builder
            .append(&h, &[][..])
            .map_err(|e| FileError::Io(e.to_string()))?;
    } else if ft.is_file() {
        let f = std::fs::File::open(&item.full).map_err(map_io)?;
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_path(&item.arc)
            .map_err(|e| FileError::Io(e.to_string()))?;
        h.set_metadata(&md);
        h.set_size(md.len());
        h.set_cksum();
        builder
            .append(&h, f)
            .map_err(|e| FileError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Pack `rel_paths` (paths relative to `root`) into a gzip-compressed tar.
/// Every name is validated with [`resolve`]; absolute or `..` names are
/// refused, so the produced archive only contains safe relative names.
pub fn export_tar_gz(root: &Path, rel_paths: &[&str]) -> Result<Vec<u8>, FileError> {
    let mut items = Vec::new();
    for rel in rel_paths {
        let full = resolve(root, rel)?;
        let arc = PathBuf::from(rel);
        collect_export(&full, &arc, &mut items)?;
    }
    let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(enc);
    for item in &items {
        // Skip the bare root entry itself (`arc == ""`): its children carry
        // top-level names.
        if item.arc.as_os_str().is_empty() {
            continue;
        }
        append_item(&mut builder, item)?;
    }
    let enc = builder
        .into_inner()
        .map_err(|e| FileError::Io(e.to_string()))?;
    enc.finish().map_err(|e| FileError::Io(e.to_string()))
}

/// Reject archive member names: must be relative with no `..` component.
fn check_archive_rel(rel: &Path) -> Result<(), FileError> {
    if rel.as_os_str().is_empty() {
        return Err(FileError::OutsideRoot);
    }
    for comp in rel.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(FileError::OutsideRoot)
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

/// Validate a symlink/hardlink target stored at archive path `link_rel`: it
/// must be relative and, joined onto the link's parent directory, must not
/// escape above the destination root (lexically normalized).
fn check_link_target(link_rel: &Path, target: &Path) -> Result<(), FileError> {
    for comp in target.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => return Err(FileError::OutsideRoot),
            _ => {}
        }
    }
    let parent = link_rel.parent().unwrap_or_else(|| Path::new(""));
    let joined = parent.join(target);
    let mut depth: i64 = 0;
    for comp in joined.components() {
        match comp {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(FileError::OutsideRoot);
                }
            }
            Component::Normal(_) => depth += 1,
            _ => {}
        }
    }
    Ok(())
}

/// Validate a hardlink target. Tar linknames for hardlinks are relative to the
/// archive root, not to the link's parent, so no `..` may climb above it.
fn check_hardlink_target(target: &Path) -> Result<(), FileError> {
    let mut depth: i64 = 0;
    for comp in target.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => return Err(FileError::OutsideRoot),
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(FileError::OutsideRoot);
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
        }
    }
    Ok(())
}

/// Every written path (and each ancestor up to `dest_full`) must exist
/// symlink-free; an existing symlink at any of these positions is refused so
/// extraction can never write through a link that escapes.
fn ensure_writable_location(dest_full: &Path, full: &Path) -> Result<(), FileError> {
    let mut cur = full;
    loop {
        if cur == dest_full {
            break;
        }
        if let Ok(md) = std::fs::symlink_metadata(cur) {
            if md.file_type().is_symlink() {
                return Err(FileError::OutsideRoot);
            }
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => break,
        }
    }
    Ok(())
}

fn remove_existing(full: &Path) -> Result<(), FileError> {
    let md = std::fs::symlink_metadata(full).map_err(map_io)?;
    if md.is_dir() && !md.file_type().is_symlink() {
        std::fs::remove_dir_all(full).map_err(map_io)?;
    } else {
        std::fs::remove_file(full).map_err(map_io)?;
    }
    Ok(())
}

/// Extract a gzip-compressed tar into `dest` (inside `root`).
///
/// Two phases: the whole archive is parsed and every entry validated
/// (relative `..`-free names, no device/fifo/socket entries, symlink and
/// hardlink targets confined to the destination, `max_files`/`max_bytes`
/// caps) *before* anything is written. Returns the number of non-directory
/// entries written (files + symlinks + hardlinks).
pub fn import_tar_gz(
    root: &Path,
    dest: &str,
    data: &[u8],
    limits: &ArchiveLimits,
) -> Result<usize, FileError> {
    let dest_full = resolve(root, dest)?;
    extract_tar_reader(&dest_full, flate2::read::GzDecoder::new(data), limits)
}

/// Streaming variant of [`import_tar_gz`] that never buffers the archive.
///
/// `reader` yields the **already decompressed** tar stream — for a `.tar.gz`
/// file, wrap a [`std::fs::File`] in a [`flate2::read::GzDecoder`] first.
/// Shares every validation and write guard with [`import_tar_gz`]; `dest` is
/// the destination directory (it is used as the root, with the archive's
/// entries extracted beneath it).
pub fn import_tar_gz_reader<R: std::io::Read>(
    reader: R,
    dest: &Path,
    limits: &ArchiveLimits,
) -> Result<usize, FileError> {
    let dest_full = resolve(dest, "")?;
    extract_tar_reader(&dest_full, reader, limits)
}

/// Shared two-phase extraction core: validate every entry from `reader`, then
/// write. See [`import_tar_gz`].
fn extract_tar_reader<R: std::io::Read>(
    dest_full: &Path,
    reader: R,
    limits: &ArchiveLimits,
) -> Result<usize, FileError> {
    // ---- Phase 1: parse + validate everything, no writes. ----
    let mut archive = tar::Archive::new(reader);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut symlinks: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut hardlinks: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut entry_count: usize = 0;
    let mut total_bytes: u64 = 0;

    let raw_entries = archive
        .entries()
        .map_err(|e| FileError::Io(e.to_string()))?;
    for raw in raw_entries {
        let mut entry = raw.map_err(|e| FileError::Io(e.to_string()))?;
        let rel: PathBuf = entry
            .path()
            .map_err(|e| FileError::Io(e.to_string()))?
            .into_owned();
        if rel.as_os_str().is_empty() || rel == Path::new(".") {
            continue; // directory entries have no payload; safe to skip
        }
        check_archive_rel(&rel)?;
        entry_count = entry_count.saturating_add(1);
        if entry_count > limits.max_files {
            return Err(FileError::TooManyEntries);
        }
        let etype = entry.header().entry_type();
        if etype.is_file() {
            // Cap allocation per entry at the remaining budget (+1 probe).
            let remaining = limits.max_bytes.saturating_sub(total_bytes);
            let mut buf = Vec::new();
            entry
                .by_ref()
                .take(remaining.saturating_add(1))
                .read_to_end(&mut buf)
                .map_err(|e| FileError::Io(e.to_string()))?;
            if buf.len() as u64 > remaining {
                return Err(FileError::TooLarge);
            }
            total_bytes += buf.len() as u64;
            files.push((rel, buf));
        } else if etype == tar::EntryType::Directory {
            dirs.push(rel);
        } else if etype == tar::EntryType::Symlink {
            let target = entry
                .link_name()
                .map_err(|e| FileError::Io(e.to_string()))?
                .ok_or_else(|| FileError::Io("symlink without target".to_string()))?
                .into_owned();
            check_link_target(&rel, &target)?;
            symlinks.push((rel, target));
        } else if etype == tar::EntryType::Link {
            let target = entry
                .link_name()
                .map_err(|e| FileError::Io(e.to_string()))?
                .ok_or_else(|| FileError::Io("hardlink without target".to_string()))?
                .into_owned();
            check_hardlink_target(&target)?;
            hardlinks.push((rel, target));
        } else {
            // Device nodes, fifos, sockets, and anything unrecognized are
            // never materialized.
            return Err(FileError::OutsideRoot);
        }
    }

    // ---- Phase 2: write. `rel` names are validated relative/`..`-free, so
    // `dest_full.join(rel)` is lexically inside `dest_full`; the remaining
    // risk (pre-existing symlinks in `dest`) is checked per path. ----
    std::fs::create_dir_all(dest_full).map_err(map_io)?;

    dirs.sort_by_key(|p| p.components().count());
    for d in &dirs {
        let full = dest_full.join(d);
        ensure_writable_location(dest_full, &full)?;
        match std::fs::symlink_metadata(&full) {
            Ok(md) if md.is_dir() && !md.file_type().is_symlink() => {}
            Ok(_) => remove_existing(&full)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&full).map_err(map_io)?;
            }
            Err(e) => return Err(map_io(e)),
        }
    }
    for (rel, bytes) in &files {
        let full = dest_full.join(rel);
        ensure_writable_location(dest_full, &full)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(map_io)?;
        }
        if std::fs::symlink_metadata(&full).is_ok() {
            remove_existing(&full)?;
        }
        let mut f = std::fs::File::create(&full).map_err(map_io)?;
        use std::io::Write as _;
        f.write_all(bytes).map_err(map_io)?;
        f.sync_all().map_err(map_io)?;
    }
    for (rel, target) in &symlinks {
        let full = dest_full.join(rel);
        ensure_writable_location(dest_full, &full)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(map_io)?;
        }
        if std::fs::symlink_metadata(&full).is_ok() {
            remove_existing(&full)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &full).map_err(map_io)?;
        #[cfg(not(unix))]
        {
            let _ = (target, &full);
            return Err(FileError::Io("symlinks unsupported".to_string()));
        }
    }
    for (rel, target) in &hardlinks {
        let full = dest_full.join(rel);
        ensure_writable_location(dest_full, &full)?;
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(map_io)?;
        }
        if std::fs::symlink_metadata(&full).is_ok() {
            remove_existing(&full)?;
        }
        let target_full = dest_full.join(target);
        ensure_writable_location(dest_full, &target_full)?;
        match std::fs::symlink_metadata(&target_full) {
            Ok(md) if md.file_type().is_file() => {}
            _ => {
                return Err(FileError::Io(
                    "hardlink target is not an extracted regular file".to_string(),
                ))
            }
        }
        std::fs::hard_link(&target_full, &full).map_err(map_io)?;
    }

    Ok(files.len() + symlinks.len() + hardlinks.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    static CTR: AtomicU64 = AtomicU64::new(0);

    struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root() -> (PathBuf, Guard) {
        let id = CTR.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("pumpkin-fs-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        (p.clone(), Guard(p))
    }

    #[test]
    fn error_codes_match_spec() {
        assert_eq!(FileError::OutsideRoot.code(), "PATH_OUTSIDE_SERVER_ROOT");
        assert_eq!(FileError::NotFound.code(), "NOT_FOUND");
        assert_eq!(FileError::TooLarge.code(), "TOO_LARGE");
        assert_eq!(FileError::TooManyEntries.code(), "TOO_LARGE");
        assert_eq!(FileError::Io("x".into()).code(), "FILE_ERROR");
    }

    #[test]
    fn dotdot_rejected() {
        let (root, _g) = temp_root();
        for bad in ["../x", "a/../../x", "/abs", "..", "a/../..", "/"] {
            assert_eq!(
                resolve(&root, bad),
                Err(FileError::OutsideRoot),
                "expected OutsideRoot for {bad:?}"
            );
        }
        // Sane paths still resolve inside the root.
        assert!(resolve(&root, "").unwrap() == root.canonicalize().unwrap());
        assert!(resolve(&root, "a/b")
            .unwrap()
            .starts_with(root.canonicalize().unwrap()));
    }

    #[tokio::test]
    async fn symlink_escape_rejected() {
        let (root, _g) = temp_root();
        let (outside, _g2) = temp_root();
        std::fs::write(outside.join("secret"), b"top-secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert_eq!(resolve(&root, "link/secret"), Err(FileError::OutsideRoot));
        assert_eq!(resolve(&root, "link"), Err(FileError::OutsideRoot));
        assert_eq!(
            read_file(&root, "link/secret", MAX_FILE_BYTES).await,
            Err(FileError::OutsideRoot)
        );
        assert_eq!(list_dir(&root, "link").await, Err(FileError::OutsideRoot));
        // A symlink pointing inside the root keeps working.
        #[cfg(unix)]
        {
            std::fs::create_dir_all(root.join("real")).unwrap();
            std::fs::write(root.join("real/a.txt"), b"hi").unwrap();
            std::os::unix::fs::symlink(root.join("real"), root.join("inlink")).unwrap();
            assert_eq!(
                read_file(&root, "inlink/a.txt", MAX_FILE_BYTES)
                    .await
                    .unwrap(),
                b"hi"
            );
        }
    }

    #[tokio::test]
    async fn write_is_atomic() {
        let (root, _g) = temp_root();
        write_file_atomic(&root, "f.txt", b"v1", MAX_FILE_BYTES)
            .await
            .unwrap();
        write_file_atomic(&root, "f.txt", b"v2-content", MAX_FILE_BYTES)
            .await
            .unwrap();
        assert_eq!(
            read_file(&root, "f.txt", MAX_FILE_BYTES).await.unwrap(),
            b"v2-content"
        );
        // No temp leftovers beside the target file.
        let entries = list_dir(&root, "").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "f.txt");
        // Oversized write is refused before touching disk.
        assert_eq!(
            write_file_atomic(&root, "big.bin", &[0u8; 16], 8).await,
            Err(FileError::TooLarge)
        );
        assert!(!root.join("big.bin").exists());
    }

    #[tokio::test]
    async fn basic_crud_roundtrip() {
        let (root, _g) = temp_root();
        create_dir(&root, "sub/nested").await.unwrap();
        write_file_atomic(&root, "sub/nested/a.txt", b"hello", MAX_FILE_BYTES)
            .await
            .unwrap();
        let entries = list_dir(&root, "sub/nested").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry_type, "file");
        assert_eq!(entries[0].size, 5);
        rename_path(&root, "sub/nested/a.txt", "sub/b.txt")
            .await
            .unwrap();
        assert_eq!(
            read_file(&root, "sub/b.txt", MAX_FILE_BYTES).await.unwrap(),
            b"hello"
        );
        remove_path(&root, "sub/nested").await.unwrap();
        assert_eq!(
            read_file(&root, "sub/nested", MAX_FILE_BYTES).await,
            Err(FileError::NotFound)
        );
        // Refuse the root itself.
        assert!(remove_path(&root, "").await.is_err());
        assert!(read_file(&root, "missing.txt", MAX_FILE_BYTES)
            .await
            .eq(&Err(FileError::NotFound)));
    }

    fn reg_header(path: &str, size: u64) -> tar::Header {
        let mut h = tar::Header::new_gnu();
        h.set_path(path).unwrap();
        h.set_size(size);
        h.set_mode(0o644);
        h.set_cksum();
        h
    }

    /// Build a raw 512-byte tar header with no client-side validation, so the
    /// test archive contains exactly the hostile bytes an attacker would send
    /// (`tar::Builder` refuses to emit `..`/absolute names).
    fn raw_header(name: &str, link: &str, kind: u8, size: u64) -> [u8; 512] {
        let mut h = [0u8; 512];
        let nb = name.as_bytes();
        assert!(nb.len() < 100, "test name too long");
        h[..nb.len()].copy_from_slice(nb);
        h[100..108].copy_from_slice(b"0000777\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[148..156].copy_from_slice(b"        ");
        h[156] = kind;
        let lb = link.as_bytes();
        assert!(lb.len() < 100, "test link target too long");
        h[157..157 + lb.len()].copy_from_slice(lb);
        h[257..262].copy_from_slice(b"ustar");
        h[262..264].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    /// Build a gzip tar containing one benign file plus several attacks, with
    /// raw headers so hostile names reach the importer unfiltered.
    fn malicious_archive() -> Vec<u8> {
        use std::io::Write as _;
        let mut raw = Vec::new();
        let mut entry = |name: &str, link: &str, kind: u8, payload: &[u8]| {
            raw.extend_from_slice(&raw_header(name, link, kind, payload.len() as u64));
            raw.extend_from_slice(payload);
            let pad = (512 - payload.len() % 512) % 512;
            raw.extend_from_slice(&vec![0u8; pad]);
        };
        // Benign entry first: proves nothing is written before full validation.
        entry("ok.txt", "", b'0', b"innocent");
        entry("../evil", "", b'0', b"evil"); // `..` escape
        entry("/abs", "", b'0', b"abs"); // absolute path
        entry("evillink", "../secret-outside", b'2', b""); // symlink escape
        entry("abslink", "/etc/passwd", b'2', b""); // absolute symlink target
        entry("dev-evil", "", b'4', b""); // block device node
        entry("fifo-evil", "", b'6', b""); // fifo node
                                           // Two zero blocks: end of archive.
        raw.extend_from_slice(&[0u8; 1024]);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn malicious_tar_rejected() {
        let (root, _g) = temp_root();
        let data = malicious_archive();
        let r = import_tar_gz(&root, "imported", &data, &DEFAULT_ARCHIVE_LIMITS);
        assert_eq!(r, Err(FileError::OutsideRoot));
        // Destination dir holds nothing (not even the benign leading entry).
        let dest = root.join("imported");
        let count = std::fs::read_dir(&dest).map(|rd| rd.count()).unwrap_or(0);
        assert_eq!(count, 0);
        // Nothing escaped next to the root either.
        assert!(!root.join("evil").exists());
        assert!(!root.join("ok.txt").exists());
    }

    #[test]
    fn archive_limits_enforced() {
        let (root, _g) = temp_root();
        // Benign two-file archive.
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut b = tar::Builder::new(enc);
        for name in ["a.txt", "b.txt"] {
            let payload = b"data";
            let h = reg_header(name, payload.len() as u64);
            b.append(&h, &payload[..]).unwrap();
        }
        let data = b.into_inner().unwrap().finish().unwrap();
        // File-count cap.
        let tight = ArchiveLimits {
            max_files: 1,
            max_bytes: u64::MAX,
        };
        assert_eq!(
            import_tar_gz(&root, "d1", &data, &tight),
            Err(FileError::TooManyEntries)
        );
        // Byte cap.
        let tight = ArchiveLimits {
            max_files: 100,
            max_bytes: 3,
        };
        assert_eq!(
            import_tar_gz(&root, "d2", &data, &tight),
            Err(FileError::TooLarge)
        );
    }

    #[test]
    fn export_round_trip() {
        let (root1, _g1) = temp_root();
        let (root2, _g2) = temp_root();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            write_file_atomic(&root1, "sub/a.txt", b"alpha", MAX_FILE_BYTES)
                .await
                .unwrap();
            write_file_atomic(&root1, "sub/b.bin", &[0u8, 1, 2, 3, 255], MAX_FILE_BYTES)
                .await
                .unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink("a.txt", root1.join("sub/link1")).unwrap();
        });
        let blob = export_tar_gz(&root1, &["sub"]).unwrap();
        assert!(!blob.is_empty());
        let n = import_tar_gz(&root2, "restored", &blob, &DEFAULT_ARCHIVE_LIMITS).unwrap();
        #[cfg(unix)]
        assert_eq!(n, 3);
        #[cfg(not(unix))]
        assert_eq!(n, 2);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            assert_eq!(
                read_file(&root2, "restored/sub/a.txt", MAX_FILE_BYTES)
                    .await
                    .unwrap(),
                b"alpha"
            );
            assert_eq!(
                read_file(&root2, "restored/sub/b.bin", MAX_FILE_BYTES)
                    .await
                    .unwrap(),
                vec![0u8, 1, 2, 3, 255]
            );
            #[cfg(unix)]
            {
                let t = std::fs::read_link(root2.join("restored/sub/link1")).unwrap();
                assert_eq!(t, PathBuf::from("a.txt"));
            }
        });
        // Export refuses unsafe names.
        assert_eq!(
            export_tar_gz(&root1, &["../x"]),
            Err(FileError::OutsideRoot)
        );
        assert_eq!(
            export_tar_gz(&root1, &["/abs"]),
            Err(FileError::OutsideRoot)
        );
    }

    #[test]
    fn hardlink_target_escaping_archive_root_is_rejected() {
        let (root, _g) = temp_root();
        // A hardlink's linkname is archive-root-relative, but validation used
        // to measure it from the link's own parent. `a/link -> ../secret.txt`
        // therefore passed validation and escaped `dest` into `root`.
        let outside = root.join("secret.txt");
        std::fs::write(&outside, b"top-secret").unwrap();

        let mut raw = Vec::new();
        raw.extend_from_slice(&raw_header("a/link", "../secret.txt", b'1', 0));
        raw.extend_from_slice(&[0u8; 1024]);
        use std::io::Write as _;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        let data = enc.finish().unwrap();

        assert_eq!(
            import_tar_gz(&root, "dest", &data, &DEFAULT_ARCHIVE_LIMITS),
            Err(FileError::OutsideRoot)
        );
        assert!(std::fs::symlink_metadata(root.join("dest/a/link")).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"top-secret");
    }

    #[test]
    fn import_refuses_symlink_planted_dest() {
        let (root, _g) = temp_root();
        let (outside, _g2) = temp_root();
        // Pre-existing symlink inside dest pointing out: archive entry
        // `sub/evil` must not be written through it.
        std::fs::create_dir_all(root.join("dest")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("dest/sub")).unwrap();
        let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut b = tar::Builder::new(enc);
        let payload = b"pwned";
        let h = reg_header("sub/evil", payload.len() as u64);
        b.append(&h, &payload[..]).unwrap();
        let data = b.into_inner().unwrap().finish().unwrap();
        assert_eq!(
            import_tar_gz(&root, "dest", &data, &DEFAULT_ARCHIVE_LIMITS),
            Err(FileError::OutsideRoot)
        );
        assert!(!outside.join("evil").exists());
    }
}
