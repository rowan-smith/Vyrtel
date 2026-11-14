//! Files of retired segments, waiting for their stream's maintenance to
//! delete them.
//!
//! Retention and compaction retire a segment rather than delete its file:
//! queries may still hold it in their snapshots and read from it. When the
//! last holder drops the segment, its path lands here, and the maintenance
//! pass deletes it and then syncs the directory, so the deletion survives a
//! power loss. A deletion that fails stays here and is retried on the next
//! pass, which reports a maintenance fault until it succeeds.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Default)]
pub(crate) struct RetiredFiles {
    pending: Mutex<Vec<PathBuf>>,
    /// Files were deleted but the directory sync after them failed.
    unsynced: AtomicBool,
}

impl RetiredFiles {
    pub(crate) fn push(&self, path: PathBuf) {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).push(path);
    }

    pub(crate) fn take(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Put back deletions that failed, for the next pass.
    pub(crate) fn retry_later(&self, mut paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).append(&mut paths);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub(crate) fn set_unsynced(&self, unsynced: bool) {
        self.unsynced.store(unsynced, Ordering::Release);
    }

    pub(crate) fn unsynced(&self) -> bool {
        self.unsynced.load(Ordering::Acquire)
    }
}
