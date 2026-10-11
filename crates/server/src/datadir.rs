//! The data directory as a whole: what a complete one contains, and the
//! check that runs before anything in it is created, locked or changed.
//!
//! Restoring a backup (REL-06) must never quietly lose part of it. Before
//! this check, a backup without `metadata/metadata.db` started with an
//! empty database (dashboards, alerts and API keys gone, a new admin
//! password generated), and a file from a newer Vyrtel was quarantined as
//! damage. Now [`check`] refuses such a directory with an explanation, and
//! leaves every file in it as it was.
//!
//! A complete directory carries a manifest, [`MANIFEST`], whose `format`
//! names the layout and file formats the directory uses. Any change to an
//! on-disk format bumps [`FORMAT`], so a backup taken by a newer Vyrtel is
//! refused by an older one before it reads a single data file.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use storage::StorageError;
use telemetry::Signal;

/// Manifest file name, at the top of the data directory.
pub const MANIFEST: &str = "vyrtel.json";

/// The data-directory format this build reads and writes: WAL file format
/// 1, WAL record version 1, event codec 1, segment format 1, metadata
/// schema up to [`metadata::SCHEMA_VERSION`].
pub const FORMAT: u32 = 1;

/// Where the metadata database lives, relative to the data directory.
pub const METADATA_DB: &str = "metadata/metadata.db";

/// Names Vyrtel creates at the top of a data directory. A directory with
/// none of them is new (it may hold unrelated entries such as `lost+found`).
const OWN_ENTRIES: &[&str] = &[MANIFEST, ".lock", "metadata", "wal", "segments", "quarantine", "indexes", "tmp"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    /// The Vyrtel version that last wrote the manifest.
    pub written_by: String,
}

impl Manifest {
    pub fn current() -> Manifest {
        Manifest { format: FORMAT, written_by: env!("CARGO_PKG_VERSION").to_string() }
    }
}

/// What [`check`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// No data directory yet, or none of Vyrtel's entries in it.
    New,
    /// Vyrtel data from before manifests (0.1.1 and earlier), or a first
    /// start that stopped before writing one. Usable; gets a manifest.
    Unversioned,
    /// A directory with a manifest this build reads.
    Versioned(Manifest),
}

/// Why a data directory was not started. Every case is detected before
/// anything in the directory changes.
#[derive(Debug)]
pub enum DataDirError {
    /// Parts of a complete data directory are missing (a partial backup or
    /// restore). Paths are relative to the data directory.
    Incomplete { dir: PathBuf, missing: Vec<String> },
    /// The manifest is there but cannot be used.
    BadManifest { dir: PathBuf, reason: String },
    /// A newer Vyrtel wrote this directory.
    NewerFormat { dir: PathBuf, found: u32, written_by: String },
    /// A data file is in a format this build cannot read.
    UnsupportedFile { dir: PathBuf, file: String, what: &'static str, found: u16, supported: u16 },
    /// The metadata database was migrated by a newer Vyrtel.
    NewerMetadata { dir: PathBuf, found: i64 },
    /// The metadata database cannot be read (not SQLite, damaged).
    BadMetadata { dir: PathBuf, reason: String },
    /// The directory could not be examined (permissions, I/O errors).
    Unreadable { dir: PathBuf, reason: String },
}

const NOTHING_CHANGED: &str = "Vyrtel did not start and changed nothing in it";
const SEE: &str = "see docs/configuration.md#backup-and-restore";

impl fmt::Display for DataDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let this = env!("CARGO_PKG_VERSION");
        match self {
            DataDirError::Incomplete { dir, missing } => write!(
                f,
                "data directory {} is incomplete: {} missing. {NOTHING_CHANGED}. If it is a restored backup, \
                 restore the whole data directory again; to start without the missing parts on purpose, create \
                 them empty (directories, or an empty metadata.db file) ({SEE})",
                dir.display(),
                list(missing)
            ),
            DataDirError::BadManifest { dir, reason } => write!(
                f,
                "data directory {} has an unusable {MANIFEST}: {reason}. {NOTHING_CHANGED}. Restore {MANIFEST} \
                 from the same backup as the rest of the directory ({SEE})",
                dir.display()
            ),
            DataDirError::NewerFormat { dir, found, written_by } => write!(
                f,
                "data directory {} has format {found} (written by Vyrtel {written_by}), but Vyrtel {this} reads \
                 only format {FORMAT}. {NOTHING_CHANGED}. Start it with Vyrtel {written_by} or newer ({SEE})",
                dir.display()
            ),
            DataDirError::UnsupportedFile { dir, file, what, found, supported } => write!(
                f,
                "data directory {} cannot be read by Vyrtel {this}: {file} uses {what} version {found}, and this \
                 version reads only {supported}. {NOTHING_CHANGED}. Start it with the Vyrtel version that wrote \
                 it, or newer ({SEE})",
                dir.display()
            ),
            DataDirError::NewerMetadata { dir, found } => write!(
                f,
                "data directory {} cannot be read by Vyrtel {this}: {METADATA_DB} has schema version {found}, and \
                 this version knows schema versions up to {}. {NOTHING_CHANGED}. Start it with the Vyrtel version \
                 that wrote it, or newer ({SEE})",
                dir.display(),
                metadata::SCHEMA_VERSION
            ),
            DataDirError::BadMetadata { dir, reason } => write!(
                f,
                "data directory {} has an unreadable {METADATA_DB}: {reason}. {NOTHING_CHANGED}. Restore it from \
                 the same backup as the rest of the directory ({SEE})",
                dir.display()
            ),
            DataDirError::Unreadable { dir, reason } => {
                write!(f, "data directory {} cannot be examined: {reason}. {NOTHING_CHANGED}", dir.display())
            }
        }
    }
}

impl std::error::Error for DataDirError {}

fn list(items: &[String]) -> String {
    match items {
        [one] => format!("{one} is"),
        _ => format!("{} are", items.join(", ")),
    }
}

/// Entries every started data directory has: the metadata database and
/// each signal's WAL and segment directories.
fn required() -> Vec<String> {
    let mut out = vec![METADATA_DB.to_string()];
    for kind in ["wal", "segments"] {
        for s in Signal::ALL {
            out.push(format!("{kind}/{}", s.as_str()));
        }
    }
    out
}

/// Decide whether `dir` can be started, reading only. Runs before the
/// data directory is created or locked, so on `Err` nothing in it has
/// changed.
///
/// Refused: a missing part of a complete directory, an unreadable or newer
/// manifest, a segment or WAL file in a newer format, and a metadata
/// database that cannot be read or has a newer schema. Damaged segments
/// and WAL tails are not refused; recovery quarantines them as after a
/// crash.
pub fn check(dir: &Path) -> Result<Layout, DataDirError> {
    let unreadable = |e: std::io::Error| DataDirError::Unreadable { dir: dir.to_path_buf(), reason: e.to_string() };
    if !dir.exists() {
        return Ok(Layout::New);
    }
    if !dir.is_dir() {
        return Err(DataDirError::Unreadable { dir: dir.to_path_buf(), reason: "not a directory".into() });
    }
    let mut own = false;
    for entry in std::fs::read_dir(dir).map_err(unreadable)? {
        let name = entry.map_err(unreadable)?.file_name();
        own |= OWN_ENTRIES.iter().any(|e| name == *e);
    }
    if !own {
        return Ok(Layout::New);
    }

    let layout = match std::fs::read(dir.join(MANIFEST)) {
        Ok(bytes) => Layout::Versioned(read_manifest(dir, &bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Layout::Unversioned,
        Err(e) => return Err(unreadable(e)),
    };

    let present = |rel: &str| dir.join(rel).exists();
    let missing: Vec<String> = match &layout {
        // A manifest is written only once every required entry exists.
        Layout::Versioned(_) => required().into_iter().filter(|r| !present(r)).collect(),
        // Without one, refuse what would quietly lose data: telemetry
        // without its metadata, or metadata without telemetry directories.
        // Neither happens on a first start that stopped early, which
        // creates the signal directories, then the database, then data.
        _ => {
            let db = present(METADATA_DB);
            if !db && has_telemetry(dir).map_err(unreadable)? {
                vec![METADATA_DB.to_string()]
            } else if db {
                required().into_iter().skip(1).filter(|r| !present(r)).collect()
            } else {
                Vec::new()
            }
        }
    };
    if !missing.is_empty() {
        return Err(DataDirError::Incomplete { dir: dir.to_path_buf(), missing });
    }

    storage::check_formats(dir).map_err(|e| match e {
        StorageError::UnsupportedFormat { path, what, found, supported } => {
            DataDirError::UnsupportedFile { dir: dir.to_path_buf(), file: relative(dir, &path), what, found, supported }
        }
        e => DataDirError::Unreadable { dir: dir.to_path_buf(), reason: e.to_string() },
    })?;

    let db = dir.join(METADATA_DB);
    if db.exists() {
        match metadata::Metadata::inspect(&db) {
            Ok(found) if found > metadata::SCHEMA_VERSION => {
                return Err(DataDirError::NewerMetadata { dir: dir.to_path_buf(), found });
            }
            Ok(_) => {}
            Err(e) => return Err(DataDirError::BadMetadata { dir: dir.to_path_buf(), reason: e.to_string() }),
        }
    }
    Ok(layout)
}

fn read_manifest(dir: &Path, bytes: &[u8]) -> Result<Manifest, DataDirError> {
    let bad = |reason: String| DataDirError::BadManifest { dir: dir.to_path_buf(), reason };
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| bad(format!("not a valid manifest ({e})")))?;
    if m.format == 0 {
        return Err(bad("format 0 does not exist".into()));
    }
    if m.format > FORMAT {
        return Err(DataDirError::NewerFormat { dir: dir.to_path_buf(), found: m.format, written_by: m.written_by });
    }
    Ok(m)
}

/// Whether any WAL or segment file exists.
fn has_telemetry(dir: &Path) -> std::io::Result<bool> {
    for kind in ["wal", "segments"] {
        for s in Signal::ALL {
            let d = dir.join(kind).join(s.as_str());
            if !d.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&d)? {
                let name = entry?.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".wal") || name.ends_with(".seg") {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn relative(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// Create each signal's WAL and segment directories, durably, so that a
/// first start interrupted before the metadata database exists is still
/// recognised as complete afterwards (see [`check`]).
pub fn prepare(dir: &Path) -> storage::Result<()> {
    for kind in ["wal", "segments"] {
        for s in Signal::ALL {
            storage::fsutil::create_dir_all_durable(&dir.join(kind).join(s.as_str()))?;
        }
    }
    Ok(())
}

/// Write the manifest once storage and metadata are open (every required
/// entry exists and the directory is locked). Skipped if `layout` already
/// carries the current one. Atomic: written to `tmp/`, fsynced, renamed.
pub fn record(dir: &Path, layout: &Layout) -> std::io::Result<()> {
    let current = Manifest::current();
    if *layout == Layout::Versioned(current.clone()) {
        return Ok(());
    }
    let tmp = dir.join("tmp").join(MANIFEST);
    {
        let mut f = std::fs::File::create(&tmp)?;
        let mut text = serde_json::to_string_pretty(&current).map_err(std::io::Error::other)?;
        text.push('\n');
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(MANIFEST))?;
    storage::fsutil::sync_dir(dir).map_err(std::io::Error::other)?;
    match layout {
        Layout::New => tracing::info!(format = FORMAT, "initialised data directory"),
        Layout::Unversioned => tracing::info!(format = FORMAT, "recorded data directory format in {MANIFEST}"),
        Layout::Versioned(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(dir: &Path) {
        prepare(dir).unwrap();
        metadata::Metadata::open(&dir.join(METADATA_DB)).unwrap();
    }

    fn write_manifest(dir: &Path, json: &str) {
        std::fs::write(dir.join(MANIFEST), json).unwrap();
    }

    #[test]
    fn new_directories() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(check(&d.path().join("absent")).unwrap(), Layout::New);
        assert_eq!(check(d.path()).unwrap(), Layout::New);
        // Entries that are not Vyrtel's do not make a directory used.
        std::fs::create_dir(d.path().join("lost+found")).unwrap();
        assert_eq!(check(d.path()).unwrap(), Layout::New);
    }

    #[test]
    fn complete_directory_with_and_without_manifest() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        assert_eq!(check(d.path()).unwrap(), Layout::Unversioned);
        std::fs::create_dir_all(d.path().join("tmp")).unwrap();
        record(d.path(), &Layout::Unversioned).unwrap();
        assert_eq!(check(d.path()).unwrap(), Layout::Versioned(Manifest::current()));
        let text = std::fs::read_to_string(d.path().join(MANIFEST)).unwrap();
        assert!(text.contains("\"format\": 1"), "{text}");
        // Unknown manifest keys from the same format are ignored.
        write_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.9", "note": "x"}"#);
        assert!(matches!(check(d.path()).unwrap(), Layout::Versioned(m) if m.written_by == "0.1.9"));
    }

    #[test]
    fn interrupted_first_start_is_not_refused() {
        let d = tempfile::tempdir().unwrap();
        // Signal directories created, metadata database not yet.
        prepare(d.path()).unwrap();
        assert_eq!(check(d.path()).unwrap(), Layout::Unversioned);
        // Only the lock file.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".lock"), b"").unwrap();
        assert_eq!(check(d.path()).unwrap(), Layout::Unversioned);
    }

    #[test]
    fn missing_parts_are_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        write_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.1"}"#);
        std::fs::remove_dir(d.path().join("wal/traces")).unwrap();
        std::fs::remove_dir(d.path().join("segments/metrics")).unwrap();
        let e = check(d.path()).unwrap_err();
        assert!(
            matches!(&e, DataDirError::Incomplete { missing, .. } if missing == &["wal/traces", "segments/metrics"]),
            "{e:?}"
        );
        let text = e.to_string();
        assert!(text.contains("is incomplete: wal/traces, segments/metrics are missing"), "{text}");
        assert!(text.contains("changed nothing"), "{text}");

        // Telemetry without metadata, with or without a manifest.
        for manifest in [true, false] {
            let d = tempfile::tempdir().unwrap();
            prepare(d.path()).unwrap();
            std::fs::write(d.path().join("wal/logs/000000000001.wal"), b"").unwrap();
            if manifest {
                write_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.1"}"#);
            }
            let text = check(d.path()).unwrap_err().to_string();
            assert!(
                text.ends_with(
                    " is incomplete: metadata/metadata.db is missing. Vyrtel did not start and changed nothing in \
                     it. If it is a restored backup, restore the whole data directory again; to start without the \
                     missing parts on purpose, create them empty (directories, or an empty metadata.db file) (see \
                     docs/configuration.md#backup-and-restore)"
                ),
                "{text}"
            );
        }

        // Metadata without telemetry directories, without a manifest.
        let d = tempfile::tempdir().unwrap();
        metadata::Metadata::open(&d.path().join(METADATA_DB)).unwrap();
        assert!(matches!(check(d.path()), Err(DataDirError::Incomplete { missing, .. }) if missing.len() == 6));

        // The documented override: create the missing parts empty.
        let d = tempfile::tempdir().unwrap();
        prepare(d.path()).unwrap();
        write_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.1"}"#);
        std::fs::create_dir_all(d.path().join("metadata")).unwrap();
        std::fs::write(d.path().join(METADATA_DB), b"").unwrap();
        assert!(matches!(check(d.path()), Ok(Layout::Versioned(_))));
    }

    #[test]
    fn manifest_problems_are_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        write_manifest(d.path(), r#"{"format": 2, "writtenBy": "0.3.0"}"#);
        let e = check(d.path()).unwrap_err();
        assert!(matches!(e, DataDirError::NewerFormat { found: 2, .. }), "{e:?}");
        let text = e.to_string();
        assert!(text.contains("has format 2 (written by Vyrtel 0.3.0)"), "{text}");
        assert!(text.contains("Start it with Vyrtel 0.3.0 or newer"), "{text}");

        for bad in ["", "{", r#"{"format": "one"}"#, r#"{"format": 0, "writtenBy": "x"}"#, r#"{"writtenBy": "x"}"#] {
            write_manifest(d.path(), bad);
            assert!(matches!(check(d.path()), Err(DataDirError::BadManifest { .. })), "{bad:?}");
        }
    }

    #[test]
    fn newer_metadata_schema_is_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        rusqlite::Connection::open(d.path().join(METADATA_DB))
            .unwrap()
            .execute("INSERT INTO schema_migrations (version, applied_at) VALUES (99, 0)", [])
            .unwrap();
        let e = check(d.path()).unwrap_err();
        assert!(matches!(e, DataDirError::NewerMetadata { found: 99, .. }), "{e:?}");
        assert!(e.to_string().contains("schema version 99"), "{e}");

        std::fs::write(d.path().join(METADATA_DB), b"definitely not sqlite, but long enough to look like a page")
            .unwrap();
        assert!(matches!(check(d.path()), Err(DataDirError::BadMetadata { .. })));
    }
}
