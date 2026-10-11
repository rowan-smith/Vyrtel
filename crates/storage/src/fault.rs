//! Storage write faults: what failed, why and where, in a form that is safe
//! to show over the API, plus test-only fault injection.
//!
//! A stream records at most one fault per [`FaultArea`]. A fault stays until
//! the same area succeeds again: a WAL write that is appended *and* synced,
//! a segment that is sealed, a maintenance pass that completes. Readiness
//! turns degraded while a WAL or segment fault is present (see
//! `docs/storage-format.md#storage-write-failures`).
//!
//! With the `fault-injection` feature, [`inject`] makes a named point fail
//! for paths below a directory, so tests can exercise full disks,
//! permission errors and short writes without touching real devices.
//! Without the feature the checks compile to nothing.
//!
//! | point           | where                                                |
//! |-----------------|------------------------------------------------------|
//! | `wal.append`    | writing a record to the active WAL                   |
//! | `wal.sync`      | fsync of the active WAL                              |
//! | `wal.create`    | creating the next WAL during rotation                |
//! | `segment.write` | writing a segment file in `tmp/` (seal, compaction)  |
//! | `segment.sync`  | fsync of a segment file before it is committed       |
//! | `probe`         | the readiness write probe in `tmp/`                  |

use std::io;
use std::path::Path;

use telemetry::Timestamp;

use crate::error::StorageError;

/// Which part of the write path failed. WAL and segment faults make the
/// ingest path unusable; maintenance faults do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultArea {
    Wal,
    Segment,
    Maintenance,
}

impl FaultArea {
    pub const ALL: [FaultArea; 3] = [FaultArea::Wal, FaultArea::Segment, FaultArea::Maintenance];

    pub fn as_str(self) -> &'static str {
        match self {
            FaultArea::Wal => "wal",
            FaultArea::Segment => "segment",
            FaultArea::Maintenance => "maintenance",
        }
    }

    /// Whether a fault here means ingest cannot meet its acknowledgement
    /// contract.
    pub fn blocks_ingest(self) -> bool {
        !matches!(self, FaultArea::Maintenance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultOp {
    WalAppend,
    WalSync,
    WalCreate,
    SegmentWrite,
    Maintenance,
    Probe,
}

impl FaultOp {
    pub fn as_str(self) -> &'static str {
        match self {
            FaultOp::WalAppend => "walAppend",
            FaultOp::WalSync => "walSync",
            FaultOp::WalCreate => "walCreate",
            FaultOp::SegmentWrite => "segmentWrite",
            FaultOp::Maintenance => "maintenance",
            FaultOp::Probe => "probe",
        }
    }

    pub fn area(self) -> FaultArea {
        match self {
            FaultOp::WalAppend | FaultOp::WalSync => FaultArea::Wal,
            // Without a new WAL the active buffer cannot be frozen and
            // sealed, so a failed create stalls the same way a failed seal does.
            FaultOp::WalCreate | FaultOp::SegmentWrite | FaultOp::Probe => FaultArea::Segment,
            FaultOp::Maintenance => FaultArea::Maintenance,
        }
    }
}

/// Why the operation failed, from the OS error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultReason {
    NoSpace,
    PermissionDenied,
    ReadOnly,
    ShortWrite,
    Io,
    Other,
}

impl FaultReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FaultReason::NoSpace => "noSpace",
            FaultReason::PermissionDenied => "permissionDenied",
            FaultReason::ReadOnly => "readOnly",
            FaultReason::ShortWrite => "shortWrite",
            FaultReason::Io => "ioError",
            FaultReason::Other => "other",
        }
    }

    /// Short phrase for client-facing messages.
    pub fn describe(self) -> &'static str {
        match self {
            FaultReason::NoSpace => "no space left on the storage device",
            FaultReason::PermissionDenied => "permission denied on the data directory",
            FaultReason::ReadOnly => "the data directory is read-only",
            FaultReason::ShortWrite => "a write to storage was cut short",
            FaultReason::Io => "an I/O error on the storage device",
            FaultReason::Other => "a storage error",
        }
    }

    pub fn from_io(e: &io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded | io::ErrorKind::FileTooLarge => {
                FaultReason::NoSpace
            }
            io::ErrorKind::PermissionDenied => FaultReason::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem => FaultReason::ReadOnly,
            io::ErrorKind::WriteZero => FaultReason::ShortWrite,
            _ => FaultReason::Io,
        }
    }
}

/// A storage write failure as operators see it. `path` is relative to the
/// data directory and `message` is the OS error text without any path, so
/// the whole value is safe to return from the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageFault {
    pub op: FaultOp,
    pub reason: FaultReason,
    pub path: String,
    pub message: String,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
    /// How many times this fault occurred since it was first seen.
    pub occurrences: u64,
}

impl StorageFault {
    /// Classify `e`. `fallback` names the location when the error itself
    /// carries no path.
    pub fn from_error(op: FaultOp, e: &StorageError, fallback: &Path, data_dir: &Path) -> Self {
        let (reason, path, message) = match e {
            StorageError::Io { path, source } => (FaultReason::from_io(source), path.as_path(), source.to_string()),
            StorageError::WriteFailed(f) => return Self { op, ..f.clone() },
            StorageError::Compression(source) => (FaultReason::from_io(source), fallback, source.to_string()),
            StorageError::Corrupt { source, .. } => (FaultReason::Other, fallback, format!("corrupt data: {source}")),
            other => (FaultReason::Other, fallback, other.to_string()),
        };
        let now = Timestamp::now();
        Self { op, reason, path: relative(path, data_dir), message, first_seen: now, last_seen: now, occurrences: 1 }
    }

    pub fn area(&self) -> FaultArea {
        self.op.area()
    }
}

impl std::fmt::Display for StorageFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed on {}: {} ({})", self.op.as_str(), self.path, self.message, self.reason.as_str())
    }
}

/// `path` relative to `data_dir` with `/` separators; just the file name if
/// it lies outside, so host paths never leave the process.
fn relative(path: &Path, data_dir: &Path) -> String {
    match path.strip_prefix(data_dir) {
        Ok(rel) => rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"),
        Err(_) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

/// Fail with an injected error if a rule matches `point` and `path`.
#[inline(always)]
pub(crate) fn check(point: &str, path: &Path) -> io::Result<()> {
    #[cfg(feature = "fault-injection")]
    if let Some(f) = inject::take(point, path) {
        return Err(f.error());
    }
    let _ = (point, path);
    Ok(())
}

/// Like [`check`] for a write of `len` bytes: returns how many bytes to
/// write before failing (a short write writes half) and the error.
#[inline(always)]
pub(crate) fn check_write(point: &str, path: &Path, len: usize) -> Option<(usize, io::Error)> {
    #[cfg(feature = "fault-injection")]
    if let Some(f) = inject::take(point, path) {
        let written = if f == inject::Injected::ShortWrite { len / 2 } else { 0 };
        return Some((written, f.error()));
    }
    let _ = (point, path, len);
    None
}

#[cfg(feature = "fault-injection")]
pub mod inject {
    //! Test-only fault injection. Rules are process-wide but scoped to a
    //! directory, so tests running in parallel on separate temp dirs do not
    //! see each other's faults.

    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Injected {
        /// `ENOSPC` / `ERROR_DISK_FULL`.
        NoSpace,
        /// `EACCES`.
        PermissionDenied,
        /// `EROFS`.
        ReadOnly,
        /// Half of the bytes are written, then the write fails.
        ShortWrite,
    }

    impl Injected {
        pub(crate) fn error(self) -> io::Error {
            match self {
                Injected::NoSpace => io::Error::new(io::ErrorKind::StorageFull, "No space left on device (injected)"),
                Injected::PermissionDenied => {
                    io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied (injected)")
                }
                Injected::ReadOnly => {
                    io::Error::new(io::ErrorKind::ReadOnlyFilesystem, "Read-only file system (injected)")
                }
                Injected::ShortWrite => io::Error::new(io::ErrorKind::WriteZero, "short write (injected)"),
            }
        }
    }

    struct Rule {
        id: u64,
        scope: PathBuf,
        point: String,
        fault: Injected,
        /// `None` fails every time.
        remaining: Option<u64>,
    }

    static RULES: Mutex<Vec<Rule>> = Mutex::new(Vec::new());
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    /// Removes its rule when dropped: storage behaves normally again.
    #[must_use = "the fault is removed when the guard is dropped"]
    pub struct FaultGuard(u64);

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            RULES.lock().unwrap_or_else(|e| e.into_inner()).retain(|r| r.id != self.0);
        }
    }

    /// Make `point` fail with `fault` for every path below `scope` until
    /// the guard is dropped.
    pub fn inject(scope: &Path, point: &str, fault: Injected) -> FaultGuard {
        add(scope, point, fault, None)
    }

    /// Make `point` fail the next `times` times only.
    pub fn inject_times(scope: &Path, point: &str, fault: Injected, times: u64) -> FaultGuard {
        add(scope, point, fault, Some(times))
    }

    fn add(scope: &Path, point: &str, fault: Injected, remaining: Option<u64>) -> FaultGuard {
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        RULES.lock().unwrap_or_else(|e| e.into_inner()).push(Rule {
            id,
            scope: scope.to_path_buf(),
            point: point.to_string(),
            fault,
            remaining,
        });
        FaultGuard(id)
    }

    pub(crate) fn take(point: &str, path: &Path) -> Option<Injected> {
        let mut rules = RULES.lock().unwrap_or_else(|e| e.into_inner());
        let rule = rules.iter_mut().find(|r| r.point == point && path.starts_with(&r.scope))?;
        match &mut rule.remaining {
            Some(0) => return None,
            Some(n) => *n -= 1,
            None => {}
        }
        Some(rule.fault)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_os_errors() {
        let r = |k| FaultReason::from_io(&io::Error::from(k));
        assert_eq!(r(io::ErrorKind::StorageFull), FaultReason::NoSpace);
        assert_eq!(r(io::ErrorKind::PermissionDenied), FaultReason::PermissionDenied);
        assert_eq!(r(io::ErrorKind::ReadOnlyFilesystem), FaultReason::ReadOnly);
        assert_eq!(r(io::ErrorKind::WriteZero), FaultReason::ShortWrite);
        assert_eq!(r(io::ErrorKind::Other), FaultReason::Io);
        #[cfg(unix)]
        assert_eq!(FaultReason::from_io(&io::Error::from_raw_os_error(28)), FaultReason::NoSpace);
        #[cfg(windows)]
        assert_eq!(FaultReason::from_io(&io::Error::from_raw_os_error(112)), FaultReason::NoSpace);
    }

    #[test]
    fn fault_paths_are_relative_and_messages_carry_no_host_path() {
        let data = Path::new("/srv/secret-customer/vyrtel");
        let file = data.join("wal").join("logs").join("000000000003.wal");
        let e = StorageError::Io { path: file.clone(), source: io::Error::from(io::ErrorKind::StorageFull) };
        let f = StorageFault::from_error(FaultOp::WalAppend, &e, &file, data);
        assert_eq!(f.path, "wal/logs/000000000003.wal");
        assert_eq!(f.reason, FaultReason::NoSpace);
        assert!(!f.message.contains("secret-customer"), "{}", f.message);
        assert!(!f.to_string().contains("secret-customer"), "{f}");

        // Outside the data directory only the file name is kept.
        let outside = Path::new("/home/alice/elsewhere/file.seg");
        let e = StorageError::Io { path: outside.into(), source: io::Error::from(io::ErrorKind::PermissionDenied) };
        let f = StorageFault::from_error(FaultOp::SegmentWrite, &e, outside, data);
        assert_eq!(f.path, "file.seg");
        assert_eq!(f.area(), FaultArea::Segment);
    }
}
