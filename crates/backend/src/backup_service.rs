//! Full-data backup core: tar.gz archive, SHA-256 checksum, retention decisions.
//!
//! Phase 5, Task 1. Pure implementation (no dependency on `file_service`).
//! `mode` is always full-data-dir in Phase 5: every backup is a tar.gz
//! snapshot of the whole data dir with safe relative entry names.

// Task 2 consumes this module's entry points; nothing in the crate wires it
// up yet, so dead-code is expected until then (remove once wired).
#![allow(dead_code)]

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

/// Manifest format version written by this module.
pub const BACKUP_VERSION: u32 = 1;

/// Manifest describing one finished `.tar.gz` backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupManifest {
    /// Number of regular files stored in the archive.
    pub files: u64,
    /// Size of the finished `.tar.gz` file in bytes.
    pub bytes: u64,
    /// Lowercase hex SHA-256 over the finished `.tar.gz` bytes.
    pub checksum_sha256: String,
}

/// Errors produced by backup core operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupError {
    /// Filesystem / archive / task failure.
    #[error("backup failed: {0}")]
    Io(String),
    /// Checksum mismatch or unreadable archive.
    #[error("backup corrupt: {0}")]
    Corrupt(String),
    /// Archive exceeds the allowed size (enforced by callers).
    #[error("backup too large")]
    TooLarge,
    /// Operation was cancelled (enforced by callers).
    #[error("task cancelled")]
    Cancelled,
}

impl BackupError {
    /// Stable machine-readable code for API responses.
    pub fn code(&self) -> &'static str {
        match self {
            BackupError::Io(_) => "BACKUP_FAILED",
            BackupError::Corrupt(_) => "BACKUP_CORRUPT",
            BackupError::TooLarge => "TOO_LARGE",
            BackupError::Cancelled => "TASK_CANCELLED",
        }
    }
}

/// One backup row as seen by the retention decision function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionInput {
    pub id: String,
    pub completed_at: Option<String>,
    pub verified: bool,
    pub protected: bool,
    pub in_progress: bool,
}

/// One DB row as needed by retention, before verification semantics are
/// applied. Shared by the preview and the worker so they cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSource {
    pub id: String,
    pub state: String,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub checksum: String,
    pub protected: bool,
}

/// Map DB rows to retention inputs with a single definition of "verified"
/// (completed with a recorded checksum) and "in progress".
#[must_use]
pub fn retention_inputs(rows: &[RetentionSource]) -> Vec<RetentionInput> {
    rows.iter()
        .map(|row| RetentionInput {
            id: row.id.clone(),
            completed_at: row
                .completed_at
                .clone()
                .or_else(|| Some(row.created_at.clone())),
            verified: row.state == "completed" && !row.checksum.is_empty(),
            protected: row.protected,
            in_progress: row.state == "in_progress",
        })
        .collect()
}

/// Decide which backups to delete. Input must be newest-first.
///
/// A backup is deletion-eligible when it is beyond `keep_count` (position
/// among non-in-progress backups) or older than `max_age_days`
/// (`max_age_days == 0` disables age expiry; unparseable/missing timestamps
/// never count as expired). In-progress and protected backups are never
/// listed. Retention floor: when at least one verified backup exists, the
/// newest verified backup slated for deletion is kept so >= 1 verified
/// backup always survives.
pub fn select_for_deletion(
    backups_newest_first: &[RetentionInput],
    keep_count: usize,
    max_age_days: u64,
    now_unix: u64,
) -> Vec<String> {
    let mut preliminary: Vec<usize> = Vec::new();
    let mut rank: usize = 0;
    for (idx, backup) in backups_newest_first.iter().enumerate() {
        if backup.in_progress {
            continue;
        }
        let beyond_keep = rank >= keep_count;
        rank += 1;
        if backup.protected {
            continue;
        }
        let expired = is_expired(backup.completed_at.as_deref(), max_age_days, now_unix);
        if beyond_keep || expired {
            preliminary.push(idx);
        }
    }
    if preliminary.is_empty() {
        return Vec::new();
    }
    let verified_total = backups_newest_first
        .iter()
        .filter(|b| !b.in_progress && b.verified)
        .count();
    if verified_total == 0 {
        return preliminary
            .into_iter()
            .map(|i| backups_newest_first[i].id.clone())
            .collect();
    }
    let slated: HashSet<usize> = preliminary.iter().copied().collect();
    let verified_retained = backups_newest_first
        .iter()
        .enumerate()
        .filter(|(i, b)| !b.in_progress && b.verified && !slated.contains(i))
        .count();
    let mut delete = preliminary;
    if verified_retained == 0 {
        // Floor hit: keep the newest verified backup slated for deletion.
        if let Some(pos) = delete
            .iter()
            .position(|&i| backups_newest_first[i].verified)
        {
            delete.remove(pos);
        }
    }
    delete
        .into_iter()
        .map(|i| backups_newest_first[i].id.clone())
        .collect()
}

/// Validate a standard 5-field cron expression (`minute hour dom month dow`).
///
/// Only exactly five whitespace-separated fields are accepted; the `cron`
/// crate parser wants a leading seconds field, so a `0` prefix is shimmed in.
/// Six/seven-field (seconds/year) forms and named aliases (`@daily`) are
/// rejected: the scheduler stores and evaluates 5-field expressions only.
pub fn cron_is_valid(expr: &str) -> bool {
    use std::str::FromStr;
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    cron::Schedule::from_str(&format!("0 {}", fields.join(" "))).is_ok()
}

/// Create a full tar.gz backup of `data_dir` at `dest_file`.
///
/// Archives every regular file (plus directory entries) under `data_dir`
/// with safe relative names. Symlinks are skipped. Calls
/// `progress(files_done, files_total)` per archived file. All blocking I/O
/// runs inside `spawn_blocking`; progress reports are relayed to the caller
/// as they arrive.
pub async fn create_full_backup(
    data_dir: &Path,
    dest_file: &Path,
    progress: &(dyn Fn(u64, u64) + Send + Sync),
) -> Result<BackupManifest, BackupError> {
    let data_dir = data_dir.to_path_buf();
    let dest_file = dest_file.to_path_buf();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();
    let mut handle = tokio::task::spawn_blocking(move || build_archive(&data_dir, &dest_file, tx));
    loop {
        tokio::select! {
            join = &mut handle => {
                while let Ok((done, total)) = rx.try_recv() {
                    progress(done, total);
                }
                let result = join
                    .map_err(|e| BackupError::Io(format!("backup task failed: {e}")))?;
                return result;
            }
            msg = rx.recv() => {
                match msg {
                    Some((done, total)) => progress(done, total),
                    None => {
                        while let Ok((done, total)) = rx.try_recv() {
                            progress(done, total);
                        }
                        let result = handle
                            .await
                            .map_err(|e| BackupError::Io(format!("backup task failed: {e}")))?;
                        return result;
                    }
                }
            }
        }
    }
}

/// Re-hash `archive` and compare against `expected_sha256`, then re-list the
/// tar.gz contents. Any mismatch or unreadable archive yields
/// [`BackupError::Corrupt`].
pub async fn verify_backup(
    archive: &Path,
    expected_sha256: &str,
) -> Result<BackupManifest, BackupError> {
    let archive = archive.to_path_buf();
    let expected = expected_sha256.to_lowercase();
    tokio::task::spawn_blocking(move || verify_blocking(&archive, &expected))
        .await
        .map_err(|e| BackupError::Io(format!("verify task failed: {e}")))?
}

fn build_archive(
    data_dir: &Path,
    dest_file: &Path,
    progress_tx: tokio::sync::mpsc::UnboundedSender<(u64, u64)>,
) -> Result<BackupManifest, BackupError> {
    if !data_dir.is_dir() {
        return Err(BackupError::Io(format!(
            "data dir not found: {}",
            data_dir.display()
        )));
    }
    if let Some(parent) = dest_file.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| BackupError::Io(format!("cannot create backup dir: {e}")))?;
        }
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    walk_tree(data_dir, data_dir, &mut dirs, &mut files)
        .map_err(|e| BackupError::Io(format!("cannot scan data dir: {e}")))?;
    // Never archive the destination file itself when it lives in the data dir.
    files.retain(|rel| data_dir.join(rel).as_path() != dest_file);
    let total = files.len() as u64;

    let out = File::create(dest_file)
        .map_err(|e| BackupError::Io(format!("cannot create backup file: {e}")))?;
    let encoder = GzEncoder::new(out, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut skipped: u64 = 0;
    for dir in &dirs {
        match builder.append_dir(dir, data_dir.join(dir)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => skipped += 1,
            Err(e) => return Err(BackupError::Io(format!("cannot archive dir: {e}"))),
        }
    }
    if total == 0 {
        let _ = progress_tx.send((0, 0));
    }
    for (i, rel) in files.iter().enumerate() {
        match builder.append_path_with_name(data_dir.join(rel), rel) {
            Ok(()) => {}
            // A live world rotates/deletes files while we walk it; a missing
            // entry is a skipped file, not a failed backup.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => skipped += 1,
            Err(e) => return Err(BackupError::Io(format!("cannot archive file: {e}"))),
        }
        let _ = progress_tx.send((i as u64 + 1, total));
    }
    if skipped > 0 {
        tracing::warn!(skipped, "backup skipped entries that vanished mid-walk");
    }
    let encoder = builder
        .into_inner()
        .map_err(|e| BackupError::Io(format!("cannot finish backup archive: {e}")))?;
    encoder
        .finish()
        .map_err(|e| BackupError::Io(format!("cannot flush backup archive: {e}")))?;

    let bytes = std::fs::metadata(dest_file)
        .map_err(|e| BackupError::Io(format!("cannot stat backup archive: {e}")))?
        .len();
    let checksum_sha256 = sha256_file(dest_file)
        .map_err(|e| BackupError::Io(format!("cannot hash backup archive: {e}")))?;
    Ok(BackupManifest {
        files: total,
        bytes,
        checksum_sha256,
    })
}

fn verify_blocking(archive: &Path, expected_sha256: &str) -> Result<BackupManifest, BackupError> {
    let bytes = std::fs::metadata(archive)
        .map_err(|e| BackupError::Io(format!("cannot stat backup archive: {e}")))?
        .len();
    let actual = sha256_file(archive)
        .map_err(|e| BackupError::Io(format!("cannot hash backup archive: {e}")))?;
    if actual != expected_sha256 {
        return Err(BackupError::Corrupt("backup checksum mismatch".to_string()));
    }
    let file = File::open(archive)
        .map_err(|e| BackupError::Io(format!("cannot open backup archive: {e}")))?;
    let mut tar = tar::Archive::new(GzDecoder::new(file));
    let entries = tar
        .entries()
        .map_err(|e| BackupError::Corrupt(format!("cannot list backup archive: {e}")))?;
    let mut files = 0u64;
    for entry in entries {
        let entry =
            entry.map_err(|e| BackupError::Corrupt(format!("cannot read backup entry: {e}")))?;
        if entry.header().entry_type().is_file() {
            files += 1;
        }
    }
    Ok(BackupManifest {
        files,
        bytes,
        checksum_sha256: actual,
    })
}

fn walk_tree(
    base: &Path,
    dir: &Path,
    dirs: &mut Vec<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(base)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            .to_path_buf();
        if !is_safe_relative(&rel) {
            continue;
        }
        if file_type.is_dir() {
            dirs.push(rel.clone());
            walk_tree(base, &entry.path(), dirs, files)?;
        } else if file_type.is_file() {
            files.push(rel);
        }
    }
    Ok(())
}

fn is_safe_relative(rel: &Path) -> bool {
    !rel.as_os_str().is_empty() && rel.components().all(|c| matches!(c, Component::Normal(_)))
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn completed_at_unix(completed_at: Option<&str>) -> Option<i64> {
    let s = completed_at?;
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    s.trim().parse::<i64>().ok()
}

fn is_expired(completed_at: Option<&str>, max_age_days: u64, now_unix: u64) -> bool {
    if max_age_days == 0 {
        return false;
    }
    let ts = match completed_at_unix(completed_at) {
        Some(ts) if ts >= 0 => ts as u64,
        _ => return false,
    };
    now_unix.saturating_sub(ts) > max_age_days.saturating_mul(86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    static TEST_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_dir(name: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "pumpkin-backup-{name}-{}-{seq}",
            std::process::id()
        ))
    }

    fn write_tree(root: &Path) {
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello pumpkin").unwrap();
        std::fs::write(root.join("sub/b.bin"), [0u8, 1, 2, 250, 255, 13, 10]).unwrap();
        std::fs::write(root.join("sub/deep/c.txt"), "nested content").unwrap();
        std::fs::create_dir_all(root.join("emptydir")).unwrap();
    }

    fn retention_row(
        id: &str,
        verified: bool,
        protected: bool,
        in_progress: bool,
    ) -> RetentionInput {
        RetentionInput {
            id: id.to_string(),
            completed_at: Some("2026-01-01T00:00:00Z".to_string()),
            verified,
            protected,
            in_progress,
        }
    }

    #[tokio::test]
    async fn round_trip_preserves_bytes() {
        let root = unique_dir("roundtrip");
        let src = root.join("src");
        write_tree(&src);
        let archive = root.join("backup.tar.gz");

        let manifest = create_full_backup(&src, &archive, &|_, _| {})
            .await
            .unwrap();
        assert_eq!(manifest.files, 3);
        assert_eq!(manifest.bytes, std::fs::metadata(&archive).unwrap().len());
        assert_eq!(manifest.checksum_sha256.len(), 64);

        let verified = verify_backup(&archive, &manifest.checksum_sha256)
            .await
            .unwrap();
        assert_eq!(verified, manifest);

        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let file = File::open(&archive).unwrap();
        tar::Archive::new(GzDecoder::new(file))
            .unpack(&out)
            .unwrap();
        for rel in ["a.txt", "sub/b.bin", "sub/deep/c.txt"] {
            assert_eq!(
                std::fs::read(src.join(rel)).unwrap(),
                std::fs::read(out.join(rel)).unwrap(),
                "{rel}"
            );
        }
        assert!(out.join("emptydir").is_dir());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn corrupt_archive_rejected() {
        let root = unique_dir("corrupt");
        let src = root.join("src");
        write_tree(&src);
        let archive = root.join("backup.tar.gz");

        let manifest = create_full_backup(&src, &archive, &|_, _| {})
            .await
            .unwrap();

        let mut bytes = std::fs::read(&archive).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&archive, &bytes).unwrap();

        let err = verify_backup(&archive, &manifest.checksum_sha256)
            .await
            .unwrap_err();
        assert!(
            matches!(err, BackupError::Corrupt(_)),
            "expected Corrupt, got {err:?}"
        );
        assert_eq!(err.code(), "BACKUP_CORRUPT");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn retention_floor_keeps_last_verified() {
        // keep_count=0 would delete everything unprotected, but the floor
        // must keep one verified backup.
        let backups = vec![
            retention_row("newest", true, false, false),
            retention_row("middle", true, false, false),
            retention_row("oldest", false, false, false),
        ];
        let delete = select_for_deletion(&backups, 0, 0, 1_800_000_000);
        assert!(!delete.contains(&"newest".to_string()), "{delete:?}");
        assert_eq!(delete, vec!["middle".to_string(), "oldest".to_string()]);

        // Protected backups are never listed.
        let backups = vec![
            retention_row("newest", true, false, false),
            retention_row("kept", true, true, false),
            retention_row("oldest", false, false, false),
        ];
        let delete = select_for_deletion(&backups, 0, 0, 1_800_000_000);
        assert!(!delete.contains(&"kept".to_string()), "{delete:?}");

        // In-progress backups are never listed.
        let backups = vec![
            retention_row("running", false, false, true),
            retention_row("done", false, false, false),
        ];
        let delete = select_for_deletion(&backups, 0, 0, 1_800_000_000);
        assert!(!delete.contains(&"running".to_string()), "{delete:?}");
        assert_eq!(delete, vec!["done".to_string()]);
    }

    #[test]
    fn retention_respects_keep_count() {
        let backups = vec![
            retention_row("b1", true, false, false),
            retention_row("b2", true, false, false),
            retention_row("b3", true, false, false),
        ];
        let delete = select_for_deletion(&backups, 2, 0, 1_800_000_000);
        assert_eq!(delete, vec!["b3".to_string()]);
        let delete = select_for_deletion(&backups, 5, 0, 1_800_000_000);
        assert!(delete.is_empty(), "{delete:?}");
    }

    #[test]
    fn bad_cron_rejected() {
        assert!(!cron_is_valid("every minute-ish"));
        assert!(!cron_is_valid(""));
        assert!(!cron_is_valid("* * *"));
        assert!(cron_is_valid("0 2 * * *"));
    }

    #[tokio::test]
    async fn progress_called() {
        let root = unique_dir("progress");
        let src = root.join("src");
        write_tree(&src);
        let archive = root.join("backup.tar.gz");

        let calls = Mutex::new(Vec::<(u64, u64)>::new());
        let manifest = create_full_backup(&src, &archive, &|done, total| {
            calls.lock().unwrap().push((done, total));
        })
        .await
        .unwrap();

        let calls = calls.lock().unwrap();
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|&(_, total)| total == manifest.files));
        let done: Vec<u64> = calls.iter().map(|&(d, _)| d).collect();
        assert_eq!(*done.last().unwrap(), manifest.files);
        let mut sorted = done.clone();
        sorted.sort_unstable();
        assert_eq!(done, sorted, "progress counts must increase");
        assert_eq!(done, vec![1, 2, 3]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn error_codes() {
        assert_eq!(BACKUP_VERSION, 1);
        assert_eq!(BackupError::Io("x".into()).code(), "BACKUP_FAILED");
        assert_eq!(BackupError::Corrupt("x".into()).code(), "BACKUP_CORRUPT");
        assert_eq!(BackupError::TooLarge.code(), "TOO_LARGE");
        assert_eq!(BackupError::Cancelled.code(), "TASK_CANCELLED");
    }
}
