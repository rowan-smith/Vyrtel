//! The open segment files: never more than [`MAX_OPEN_SEGMENT_FILES`] at once.
//!
//! A segment is only read when a query or compaction needs one of its
//! regions, so holding every segment's file open made the process's file
//! descriptors grow with retained data until the open-file limit stopped
//! ingestion. Instead, every segment file is opened through this module and
//! counts against one budget until it is closed, whether the cache holds it
//! or a read is using it after the cache let it go:
//!
//! * A read takes a handle from a least-recently-used cache, opening the file
//!   on a miss. A miss first reserves a place in the budget: if none is free,
//!   it closes the least recently used cached file that no read is using,
//!   and if every open file is in use, it waits until one closes.
//! * A read holds at most one handle and never waits while holding it, so a
//!   wait always ends: whoever holds the files it waits for is reading, not
//!   waiting.
//! * A handle closes its file before giving its place back, so the count of
//!   open descriptors never exceeds the budget, even for a moment.
//!
//! Handles are keyed by path, so every `Storage` in a process shares the
//! budget.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

/// Segment files open at once. Leaves room under a 1,024 open-file limit, and
/// under the transport budget's 128 descriptors besides connections.
pub const MAX_OPEN_SEGMENT_FILES: usize = 64;

pub(crate) static OPEN_FILES: LazyLock<OpenFiles> = LazyLock::new(|| OpenFiles::new(MAX_OPEN_SEGMENT_FILES));

/// An open segment file. It counts against the budget until its last clone
/// drops, which closes the file and then frees its place.
#[derive(Debug)]
pub(crate) struct SegmentFile {
    file: Option<File>,
    budget: Arc<Budget>,
}

impl Deref for SegmentFile {
    type Target = File;
    fn deref(&self) -> &File {
        self.file.as_ref().expect("open until dropped")
    }
}

impl Drop for SegmentFile {
    fn drop(&mut self) {
        drop(self.file.take()); // close first ...
        self.budget.release(); // ... then make room for another.
    }
}

#[derive(Debug)]
struct Budget {
    cap: usize,
    open: Mutex<Counts>,
    freed: Condvar,
}

#[derive(Debug, Default)]
struct Counts {
    open: usize,
    peak: usize,
}

impl Budget {
    fn try_reserve(&self) -> bool {
        let mut c = self.open.lock().unwrap();
        if c.open >= self.cap {
            return false;
        }
        c.open += 1;
        c.peak = c.peak.max(c.open);
        true
    }

    /// Wait (at most `timeout`) for a file to close, if none has meanwhile.
    fn wait(&self, timeout: Duration) {
        let c = self.open.lock().unwrap();
        if c.open >= self.cap {
            drop(self.freed.wait_timeout(c, timeout));
        }
    }

    fn release(&self) {
        self.open.lock().unwrap().open -= 1;
        self.freed.notify_one();
    }
}

pub(crate) struct OpenFiles {
    budget: Arc<Budget>,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    files: HashMap<PathBuf, (Arc<SegmentFile>, u64)>,
    tick: u64,
}

impl Cache {
    fn touch(&mut self, path: &Path) -> Option<Arc<SegmentFile>> {
        self.tick += 1;
        let tick = self.tick;
        self.files.get_mut(path).map(|(f, used)| {
            *used = tick;
            Arc::clone(f)
        })
    }

    /// Remove the least recently used file no read is using, if any.
    fn take_idle(&mut self) -> Option<Arc<SegmentFile>> {
        let path = self
            .files
            .iter()
            .filter(|(_, (f, _))| Arc::strong_count(f) == 1)
            .min_by_key(|(_, (_, used))| *used)
            .map(|(p, _)| p.clone())?;
        self.files.remove(&path).map(|(f, _)| f)
    }
}

impl OpenFiles {
    pub(crate) fn new(cap: usize) -> Self {
        let budget = Budget { cap: cap.max(1), open: Mutex::new(Counts::default()), freed: Condvar::new() };
        Self { budget: Arc::new(budget), cache: Mutex::new(Cache::default()) }
    }

    /// A handle to the file at `path`, opened (within the budget) if it is
    /// not open already.
    pub(crate) fn get(&self, path: &Path) -> io::Result<Arc<SegmentFile>> {
        if let Some(f) = self.cache.lock().unwrap().touch(path) {
            return Ok(f);
        }
        let opened = self.open_counted(path)?;
        let mut cache = self.cache.lock().unwrap();
        // Another read may have opened it meanwhile: keep theirs, and close
        // this one once the lock is released.
        if let Some(theirs) = cache.touch(path) {
            drop(cache);
            drop(opened);
            return Ok(theirs);
        }
        let tick = cache.tick;
        cache.files.insert(path.to_path_buf(), (Arc::clone(&opened), tick));
        Ok(opened)
    }

    /// Open `path` within the budget without caching it, for a one-off read
    /// such as a format check.
    pub(crate) fn open_uncached(&self, path: &Path) -> io::Result<Arc<SegmentFile>> {
        self.open_counted(path)
    }

    fn open_counted(&self, path: &Path) -> io::Result<Arc<SegmentFile>> {
        while !self.budget.try_reserve() {
            // Taken out under the cache lock, closed after it is released.
            let idle = self.cache.lock().unwrap().take_idle();
            match idle {
                Some(f) => drop(f),
                None => self.budget.wait(Duration::from_millis(50)),
            }
        }
        match File::open(path) {
            Ok(file) => Ok(Arc::new(SegmentFile { file: Some(file), budget: Arc::clone(&self.budget) })),
            Err(e) => {
                self.budget.release();
                Err(e)
            }
        }
    }

    /// Stop caching `path`; a read using it keeps its handle until it is done.
    pub(crate) fn forget(&self, path: &Path) {
        let removed = self.cache.lock().unwrap().files.remove(path);
        drop(removed);
    }

    /// Files the cache holds now.
    #[cfg(test)]
    pub(crate) fn cached(&self) -> usize {
        self.cache.lock().unwrap().files.len()
    }

    /// Segment files open now (cached or in use), and the most at once.
    #[cfg(test)]
    pub(crate) fn open_and_peak(&self) -> (usize, usize) {
        let c = self.budget.open.lock().unwrap();
        (c.open, c.peak)
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

    fn byte(f: &File) -> u8 {
        let mut b = [0u8; 1];
        crate::fsutil::read_exact_at(f, &mut b, 0).unwrap();
        b[0]
    }

    #[test]
    fn holds_at_most_the_cap_and_closes_the_least_recently_used() {
        let (_d, paths) = files(5);
        let open = OpenFiles::new(3);
        for p in &paths[..3] {
            open.get(p).unwrap();
        }
        open.get(&paths[0]).unwrap(); // 0 is now the most recent; 1 the least.
        open.get(&paths[3]).unwrap();
        assert_eq!(open.open_and_peak(), (3, 3));
        let held: Vec<bool> = paths.iter().map(|p| open.cache.lock().unwrap().files.contains_key(p)).collect();
        assert_eq!(held, [true, false, true, true, false]);
        for p in &paths {
            assert_eq!(byte(&open.get(p).unwrap()), byte(&File::open(p).unwrap()));
        }
        assert_eq!(open.open_and_peak().1, 3, "never more than the cap open");
    }

    #[test]
    fn files_in_use_count_and_a_miss_waits_for_one_to_close() {
        let (_d, paths) = files(3);
        let open = Arc::new(OpenFiles::new(2));
        let a = open.get(&paths[0]).unwrap();
        let b = open.get(&paths[1]).unwrap();
        // Both open files are in use: nothing idle to close, so a third waits.
        let waiter = {
            let open = Arc::clone(&open);
            let p = paths[2].clone();
            std::thread::spawn(move || byte(&open.get(&p).unwrap()))
        };
        std::thread::sleep(Duration::from_millis(150));
        assert!(!waiter.is_finished(), "a third file opened while two were in use");
        assert_eq!(open.open_and_peak(), (2, 2));
        drop(a); // still cached; idle now, so the waiter closes it
        assert_eq!(waiter.join().unwrap(), 2);
        drop(b);
        assert_eq!(open.open_and_peak().1, 2);
    }

    #[test]
    fn concurrent_readers_never_exceed_the_cap() {
        let (_d, paths) = files(40);
        let open = Arc::new(OpenFiles::new(4));
        let readers: Vec<_> = (0..12)
            .map(|t| {
                let (open, paths) = (Arc::clone(&open), paths.clone());
                std::thread::spawn(move || {
                    for i in 0..400 {
                        let p = &paths[(i * 7 + t * 13) % paths.len()];
                        let f = open.get(p).unwrap();
                        assert_eq!(byte(&f), ((i * 7 + t * 13) % paths.len()) as u8);
                    }
                })
            })
            .collect();
        for r in readers {
            r.join().unwrap();
        }
        let (now, peak) = open.open_and_peak();
        assert!(peak <= 4, "peak {peak} open with a cap of 4");
        assert_eq!(now, open.cached());
    }

    #[test]
    fn uncached_opens_count_too() {
        let (_d, paths) = files(2);
        let open = OpenFiles::new(1);
        let f = open.open_uncached(&paths[0]).unwrap();
        assert_eq!(open.open_and_peak(), (1, 1));
        assert_eq!(open.cached(), 0);
        drop(f);
        assert_eq!(open.open_and_peak(), (0, 1));
        open.get(&paths[1]).unwrap();
        assert_eq!(open.open_and_peak(), (1, 1));
    }

    #[test]
    fn forget_closes_and_a_later_get_reopens() {
        let (_d, paths) = files(1);
        let open = OpenFiles::new(4);
        let first = open.get(&paths[0]).unwrap();
        open.forget(&paths[0]);
        assert_eq!(open.cached(), 0);
        assert_eq!(open.open_and_peak().0, 1, "the read still holds it");
        drop(first);
        assert_eq!(open.open_and_peak().0, 0);
        let second = open.get(&paths[0]).unwrap();
        assert_eq!(byte(&second), 0);
    }

    #[test]
    fn a_missing_file_is_an_error_and_frees_its_place() {
        let d = tempfile::tempdir().unwrap();
        let open = OpenFiles::new(1);
        assert_eq!(open.get(&d.path().join("gone.seg")).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(open.open_and_peak().0, 0);
        assert_eq!(open.cached(), 0);
    }
}
