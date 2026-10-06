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
