//! Small filesystem helpers whose behaviour matters for crash safety.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::error::{IoContext, Result};

/// Atomically move `from` to `to` (same filesystem). This is the commit
/// point for WAL headers, segments and compaction output.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).ctx(to)
}

/// Make a directory entry change (create/rename/delete) durable.
///
/// POSIX requires an fsync of the *directory* for that. Windows does not
/// allow opening directories this way and NTFS journals metadata, so it is
/// a no-op there.
pub fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(dir).and_then(|f| f.sync_all()).ctx(dir)?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Create `dir` and any missing ancestors, fsyncing the parent of every
/// directory it creates. Without that, a power loss can drop a freshly
/// created directory — and every file already made durable inside it.
pub fn create_dir_all_durable(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    let parent = dir.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(p) = parent {
        create_dir_all_durable(p)?;
    }
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        // Another thread or process won the race; it owns the parent sync.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => return Ok(()),
        Err(e) => return Err(e).ctx(dir),
    }
    sync_dir(parent.unwrap_or(Path::new(".")))
}

/// Positional read that does not move a shared cursor, so many queries can
/// read the same segment file concurrently through one handle.
pub fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short positional read"));
            }
            done += n;
        }
        Ok(())
    }
}

pub fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).ctx(path),
    }
}

/// Total size of regular files below `dir` (non-recursive into symlinks).
pub fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            // Stat the path rather than using the directory entry: on
            // Windows the entry's size lags for files still open for write.
            Ok(t) if t.is_file() => std::fs::metadata(e.path()).map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_dir_all_durable_creates_missing_ancestors_and_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let nested = d.path().join("quarantine").join("logs");
        create_dir_all_durable(&nested).unwrap();
        assert!(nested.is_dir());
        // Existing directories are left alone.
        std::fs::write(nested.join("evidence"), b"x").unwrap();
        create_dir_all_durable(&nested).unwrap();
        assert_eq!(std::fs::read(nested.join("evidence")).unwrap(), b"x");
        // A file in the way is an error, not silently accepted.
        let blocked = d.path().join("evidence-file");
        std::fs::write(&blocked, b"x").unwrap();
        assert!(create_dir_all_durable(&blocked.join("sub")).is_err());
    }
}
