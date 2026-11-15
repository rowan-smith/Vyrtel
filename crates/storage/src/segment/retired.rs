//! Segments retired by retention or compaction, from retirement until their
//! file's deletion is durable.
//!
//! A retired segment stays readable to queries that still hold it. When the
//! last holder drops it, its file lands in the queue here, and the stream's
//! maintenance deletes it and then syncs the directory, so the deletion
//! survives a power loss. A deletion or sync that fails is kept and retried
//! on the next pass, which reports a maintenance fault until it succeeds.
//!
//! Until its deletion is durable a retired segment is **outstanding**: it
//! could still be on disk after a crash. Compaction must not merge a segment
//! whose `replaces` names an outstanding one, or deleting that merged
//! segment would lose the only record that the outstanding one's events
//! were replaced, and recovery would show them twice (see
//! [`RetiredFiles::any_outstanding`]).
//!
//! Outstanding segments are bounded (by default [`MAX_OUTSTANDING`]): each
//! retention or compaction batch retires at most the remaining
//! [`capacity`](RetiredFiles::capacity), so a deletion that keeps failing
//! cannot grow this without limit, not even within one pass.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// Retired segments not yet durably deleted, at most. A few hundred KiB of
/// paths; far more than a healthy store ever has waiting.
pub const MAX_OUTSTANDING: usize = 4096;

#[derive(Debug)]
pub(crate) struct RetiredFiles {
    limit: usize,
    inner: Mutex<Inner>,
}

impl Default for RetiredFiles {
    fn default() -> Self {
        Self::new(MAX_OUTSTANDING)
    }
}

#[derive(Debug, Default)]
struct Inner {
    /// Retired and not yet durably deleted, by segment id.
    outstanding: HashSet<u64>,
    /// Released by every holder: ready to delete.
    queued: Vec<(u64, PathBuf)>,
    /// Deleted, but the directory sync after the deletion failed.
    unsynced: Vec<u64>,
}

impl RetiredFiles {
    pub(crate) fn new(limit: usize) -> Self {
        Self { limit: limit.max(1), inner: Mutex::default() }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Segment `id` was retired. Callers retire at most [`capacity`](Self::capacity).
    pub(crate) fn retire(&self, id: u64) {
        let mut inner = self.lock();
        inner.outstanding.insert(id);
        debug_assert!(inner.outstanding.len() <= self.limit, "retired past the bound");
    }

    /// Nothing holds retired segment `id` any more: its file can go.
    pub(crate) fn released(&self, id: u64, path: PathBuf) {
        self.lock().queued.push((id, path));
    }

    /// How many more segments may be retired now.
    pub(crate) fn capacity(&self) -> usize {
        self.limit.saturating_sub(self.lock().outstanding.len())
    }

    /// Segments retired and not yet durably deleted.
    pub(crate) fn outstanding_count(&self) -> usize {
        self.lock().outstanding.len()
    }

    /// Whether any of `ids` is retired and not yet durably deleted.
    pub(crate) fn any_outstanding(&self, ids: &[u64]) -> bool {
        let inner = self.lock();
        ids.iter().any(|id| inner.outstanding.contains(id))
    }

    pub(crate) fn take(&self) -> Vec<(u64, PathBuf)> {
        std::mem::take(&mut self.lock().queued)
    }

    /// Put back deletions that failed, for the next pass.
    pub(crate) fn retry_later(&self, mut failed: Vec<(u64, PathBuf)>) {
        self.lock().queued.append(&mut failed);
    }

    /// Whether deletions are waiting for a directory sync.
    pub(crate) fn unsynced(&self) -> bool {
        !self.lock().unsynced.is_empty()
    }

    /// The directory sync after deleting `ids` (and any earlier unsynced
    /// deletions) succeeded or failed.
    pub(crate) fn synced(&self, ids: Vec<u64>, ok: bool) {
        let mut inner = self.lock();
        inner.unsynced.extend(ids);
        if ok {
            let done = std::mem::take(&mut inner.unsynced);
            for id in done {
                inner.outstanding.remove(&id);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().queued.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_segment_is_outstanding_until_its_deletion_is_synced() {
        let r = RetiredFiles::default();
        r.retire(1);
        assert!(r.any_outstanding(&[7, 1]));
        r.released(1, PathBuf::from("1.seg"));
        assert_eq!(r.take(), [(1, PathBuf::from("1.seg"))]);
        r.synced(vec![1], false);
        assert!(r.any_outstanding(&[1]), "deleted but not synced: could come back");
        assert!(r.unsynced());
        r.synced(vec![], true);
        assert!(!r.any_outstanding(&[1]));
        assert!(!r.unsynced());
        assert_eq!(r.outstanding_count(), 0);
    }

    #[test]
    fn capacity_is_what_is_left_under_the_limit() {
        let r = RetiredFiles::new(3);
        assert_eq!(r.capacity(), 3);
        r.retire(1);
        r.retire(2);
        assert_eq!(r.capacity(), 1);
        r.retire(3);
        assert_eq!(r.capacity(), 0);
        r.released(1, PathBuf::from("1.seg"));
        r.synced(vec![1], true);
        assert_eq!(r.capacity(), 1, "a durable deletion frees a place");
    }
}
