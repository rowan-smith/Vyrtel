//! The open segment files, at most [`MAX_OPEN_SEGMENT_FILES`] at a time.
//!
//! A segment is only read when a query or compaction needs one of its
//! regions, so holding every segment's file open made the process's file
//! descriptors grow with retained data until the open-file limit stopped
//! ingestion. Reads instead take a handle from this least-recently-used set,
//! which opens the file on a miss and closes the least recently used one
//! beyond the cap. A read keeps its `Arc<File>` until it finishes, so
//! eviction never closes a file mid-read; while reads are in flight the
//! process can briefly hold a few more than the cap.
//!
//! Handles are keyed by path, so every `Storage` in a process shares the cap.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// Segment files kept open. Leaves room under a 1,024 open-file limit, and
/// under the transport budget's 128 descriptors besides connections.
pub const MAX_OPEN_SEGMENT_FILES: usize = 64;

pub(crate) static OPEN_FILES: LazyLock<OpenFiles> = LazyLock::new(|| OpenFiles::new(MAX_OPEN_SEGMENT_FILES));

pub(crate) struct OpenFiles {
    cap: usize,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    files: HashMap<PathBuf, (Arc<File>, u64)>,
    tick: u64,
}

impl Inner {
    fn touch(&mut self, path: &Path) -> Option<Arc<File>> {
        self.tick += 1;
        let tick = self.tick;
        self.files.get_mut(path).map(|(f, used)| {
            *used = tick;
            Arc::clone(f)
        })
    }

    fn put(&mut self, path: &Path, file: File, cap: usize) -> Arc<File> {
        self.tick += 1;
        let tick = self.tick;
        let (f, used) = self.files.entry(path.to_path_buf()).or_insert_with(|| (Arc::new(file), tick));
        *used = tick;
        let f = Arc::clone(f);
        while self.files.len() > cap {
            // A linear scan: the cap is small and this runs only on a miss.
            let oldest = self.files.iter().min_by_key(|(_, (_, used))| *used).map(|(p, _)| p.clone());
            match oldest {
                Some(p) => drop(self.files.remove(&p)),
                None => break,
            }
        }
        f
    }
}

impl OpenFiles {
    pub(crate) fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), inner: Mutex::new(Inner::default()) }
    }

    /// A handle to the file at `path`, opened if it is not open already.
    pub(crate) fn get(&self, path: &Path) -> io::Result<Arc<File>> {
        if let Some(f) = self.inner.lock().unwrap().touch(path) {
            return Ok(f);
        }
        // Opened outside the lock; if another reader opened it meanwhile,
        // theirs is kept and this one closes.
        let file = File::open(path)?;
        Ok(self.inner.lock().unwrap().put(path, file, self.cap))
    }

    /// Keep an already open `file` for `path`, e.g. the one a segment was
    /// validated with, so its first read does not open it again.
    pub(crate) fn keep(&self, path: &Path, file: File) {
        self.inner.lock().unwrap().put(path, file, self.cap);
    }

    /// Stop holding `path` open (in-flight reads keep their handle).
    pub(crate) fn forget(&self, path: &Path) {
        let removed = self.inner.lock().unwrap().files.remove(path);
        drop(removed);
    }

    /// Files held open now.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().unwrap().files.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(n: usize) -> (tempfile::TempDir, Vec<PathBuf>) {
        let d = tempfile::tempdir().unwrap();
        let paths = (0..n)
            .map(|i| {
                let p = d.path().join(format!("{i}.seg"));
                std::fs::write(&p, [i as u8]).unwrap();
                p
            })
            .collect();
        (d, paths)
    }

    #[test]
    fn holds_at_most_the_cap_and_evicts_the_least_recently_used() {
        let (_d, paths) = files(5);
        let open = OpenFiles::new(3);
        for p in &paths[..3] {
            open.get(p).unwrap();
        }
        open.get(&paths[0]).unwrap(); // 0 is now the most recent; 1 the least.
        open.get(&paths[3]).unwrap();
        assert_eq!(open.len(), 3);
        let held: Vec<bool> = paths.iter().map(|p| open.inner.lock().unwrap().files.contains_key(p)).collect();
        assert_eq!(held, [true, false, true, true, false]);
        for p in &paths {
            open.get(p).unwrap();
            assert!(open.len() <= 3);
        }
    }

    #[test]
    fn a_cached_file_is_shared_and_an_evicted_one_still_reads() {
        let (_d, paths) = files(2);
        let open = OpenFiles::new(1);
        let a = open.get(&paths[0]).unwrap();
        assert!(Arc::ptr_eq(&a, &open.get(&paths[0]).unwrap()));
        open.get(&paths[1]).unwrap(); // evicts paths[0] from the set...
        assert_eq!(open.len(), 1);
        let mut b = [0u8; 1];
        crate::fsutil::read_exact_at(&a, &mut b, 0).unwrap(); // ...but the reader's handle stays usable.
        assert_eq!(b, [0]);
    }

    #[test]
    fn forget_closes_and_a_later_get_reopens() {
        let (_d, paths) = files(1);
        let open = OpenFiles::new(4);
        let first = open.get(&paths[0]).unwrap();
        open.forget(&paths[0]);
        assert_eq!(open.len(), 0);
        let second = open.get(&paths[0]).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn a_missing_file_is_an_error_and_is_not_cached() {
        let d = tempfile::tempdir().unwrap();
        let open = OpenFiles::new(4);
        assert_eq!(open.get(&d.path().join("gone.seg")).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(open.len(), 0);
    }
}
