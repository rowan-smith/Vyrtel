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
//!
//! A clean shutdown adds an [`Inventory`] to the manifest: every WAL and
//! segment file with its size, as they are while the server is stopped. A
//! backup of the stopped directory carries it, so a restore that is missing
//! or truncates any of those files is refused. The next start verifies it
//! and removes it before changing anything, because from then on the files
//! change. Missing parts are accepted only when the operator says so
//! (`accept_incomplete`, the `--accept-incomplete-restore` flag), never
//! because a file or directory happens to exist empty.

use std::collections::BTreeMap;
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

/// How many problems a [`DataDirError::Incomplete`] message lists.
const LISTED: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    /// The Vyrtel version that last wrote the manifest.
    pub written_by: String,
    /// Present only while the directory is stopped after a clean shutdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Inventory>,
}

/// The telemetry files of a cleanly stopped data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    /// Path relative to the data directory (`wal/logs/000000000003.wal`)
    /// → size in bytes.
    pub files: BTreeMap<String, u64>,
}

impl Manifest {
    pub fn current() -> Manifest {
        Manifest { format: FORMAT, written_by: env!("CARGO_PKG_VERSION").to_string(), inventory: None }
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

/// The result of a successful [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub layout: Layout,
    /// Problems that would have refused the start, accepted on request
    /// (`accept_incomplete`). Empty otherwise.
    pub accepted: Vec<String>,
}

/// Why a data directory was not started. Every case is detected before
/// anything in the directory changes.
#[derive(Debug)]
pub enum DataDirError {
    /// Parts of a complete data directory are missing, truncated or empty
    /// (a partial backup or restore). Each problem names a path relative to
    /// the data directory.
    Incomplete { dir: PathBuf, problems: Vec<String> },
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
            DataDirError::Incomplete { dir, problems } => write!(
                f,
                "data directory {} is incomplete: {}. {NOTHING_CHANGED}. If it is a restored backup, restore the \
                 whole data directory again; to start without the missing parts on purpose, start once with \
                 --accept-incomplete-restore ({SEE})",
                dir.display(),
                list(problems)
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
    let mut text = items.iter().take(LISTED).cloned().collect::<Vec<_>>().join("; ");
    if items.len() > LISTED {
        text.push_str(&format!("; and {} more", items.len() - LISTED));
    }
    text
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

/// Decide whether `dir` can be started, reading only. Runs before the data
/// directory is created or locked, so on `Err` nothing in it has changed.
///
/// Refused as incomplete, unless `accept_incomplete` (then the problems are
/// returned in [`Checked::accepted`]): a missing required entry, a file in
/// the inventory that is missing or has another size, and a metadata
/// database without a schema where one must exist. Always refused: an
/// unreadable or newer manifest, a segment or WAL file in a newer format, a
/// metadata database that cannot be read or has a newer schema. Damaged
/// segments and WAL tails are not refused; recovery quarantines them as
/// after a crash.
pub fn check(dir: &Path, accept_incomplete: bool) -> Result<Checked, DataDirError> {
    let unreadable = |e: std::io::Error| DataDirError::Unreadable { dir: dir.to_path_buf(), reason: e.to_string() };
    let new = Checked { layout: Layout::New, accepted: Vec::new() };
    if !dir.exists() {
        return Ok(new);
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
        return Ok(new);
    }

    let layout = match std::fs::read(dir.join(MANIFEST)) {
        Ok(bytes) => Layout::Versioned(read_manifest(dir, &bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Layout::Unversioned,
        Err(e) => return Err(unreadable(e)),
    };

    let present = |rel: &str| dir.join(rel).exists();
    let missing = |rel: String| format!("{rel} is missing");
    let mut problems: Vec<String> = match &layout {
        // A manifest is written only once every required entry exists.
        Layout::Versioned(_) => required().into_iter().filter(|r| !present(r)).map(missing).collect(),
        // Without one, refuse what would quietly lose data: telemetry
        // without its metadata, or metadata without telemetry directories.
        // Neither happens on a first start that stopped early, which
        // creates the signal directories, then the database, then data.
        _ => {
            let db = present(METADATA_DB);
            if !db && has_telemetry(dir).map_err(unreadable)? {
                vec![missing(METADATA_DB.to_string())]
            } else if db {
                required().into_iter().skip(1).filter(|r| !present(r)).map(missing).collect()
            } else {
                Vec::new()
            }
        }
    };
    if let Layout::Versioned(Manifest { inventory: Some(inventory), .. }) = &layout {
        for (rel, &expected) in &inventory.files {
            match std::fs::metadata(dir.join(rel)) {
                Ok(m) if m.len() == expected => {}
                Ok(m) => problems.push(format!("{rel} has {} bytes, {expected} at the last clean shutdown", m.len())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => problems.push(missing(rel.clone())),
                Err(e) => return Err(unreadable(e)),
            }
        }
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
            // Every start migrates the database before it writes telemetry
            // or a manifest, so an empty one next to either was truncated or
            // replaced: its dashboards, alerts, users and keys are gone.
            Ok(0) if matches!(layout, Layout::Versioned(_)) || has_telemetry(dir).map_err(unreadable)? => {
                problems.push(format!("{METADATA_DB} is empty (it has no schema)"));
            }
            Ok(_) => {}
            Err(e) => return Err(DataDirError::BadMetadata { dir: dir.to_path_buf(), reason: e.to_string() }),
        }
    }

    if !problems.is_empty() && !accept_incomplete {
        return Err(DataDirError::Incomplete { dir: dir.to_path_buf(), problems });
    }
    Ok(Checked { layout, accepted: problems })
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
    for rel in m.inventory.iter().flat_map(|i| i.files.keys()) {
        if !is_telemetry_path(rel) {
            return Err(bad(format!("its inventory lists {rel:?}, which is not a WAL or segment file")));
        }
    }
    Ok(m)
}

/// `wal/<signal>/<id>.wal` or `segments/<signal>/<id>.seg`.
fn is_telemetry_path(rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let [kind, signal, name] = parts[..] else { return false };
    Signal::ALL.iter().any(|s| s.as_str() == signal)
        && match kind {
            "wal" => storage::wal::parse_wal_file_name(name).is_some(),
            "segments" => storage::segment::parse_segment_file_name(name).is_some(),
            _ => false,
        }
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

/// Before anything else in the directory changes, once [`check`] passed:
/// drop the inventory. The files are about to change, so it would no longer
/// describe them, and a crash later must not make the next start refuse
/// the directory.
pub fn begin(dir: &Path, checked: &Checked) -> std::io::Result<()> {
    if let Layout::Versioned(m) = &checked.layout
        && m.inventory.is_some()
    {
        write_manifest(dir, &Manifest::current())?;
    }
    Ok(())
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
/// entry exists and the directory is locked), unless it is already current.
pub fn record(dir: &Path, layout: &Layout) -> std::io::Result<()> {
    let current = Manifest::current();
    let on_disk = std::fs::read(dir.join(MANIFEST)).ok().and_then(|b| serde_json::from_slice::<Manifest>(&b).ok());
    if on_disk.as_ref() == Some(&current) {
        return Ok(());
    }
    write_manifest(dir, &current)?;
    match layout {
        Layout::New => tracing::info!(format = FORMAT, "initialised data directory"),
        Layout::Unversioned => tracing::info!(format = FORMAT, "recorded data directory format in {MANIFEST}"),
        Layout::Versioned(_) => {}
    }
    Ok(())
}

/// After a clean shutdown (every WAL flushed, no seal or compaction
/// running, nothing left to write): record every WAL and segment file with
/// its size, so a backup of the stopped directory can be checked for
/// completeness when it is started.
pub fn record_stopped(dir: &Path) -> std::io::Result<()> {
    let mut files = BTreeMap::new();
    for kind in ["wal", "segments"] {
        for s in Signal::ALL {
            for entry in std::fs::read_dir(dir.join(kind).join(s.as_str()))? {
                let entry = entry?;
                let rel = format!("{kind}/{}/{}", s.as_str(), entry.file_name().to_string_lossy());
                if entry.file_type()?.is_file() && is_telemetry_path(&rel) {
                    // Stat the path: on Windows the entry's size can lag.
                    files.insert(rel, std::fs::metadata(entry.path())?.len());
                }
            }
        }
    }
    let n = files.len();
    write_manifest(dir, &Manifest { inventory: Some(Inventory { files }), ..Manifest::current() })?;
    tracing::debug!(files = n, "recorded the data directory inventory");
    Ok(())
}

/// Replace the manifest atomically: write a temporary file next to it,
/// fsync it, rename it over the manifest, fsync the directory.
fn write_manifest(dir: &Path, m: &Manifest) -> std::io::Result<()> {
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        let mut text = serde_json::to_string_pretty(m).map_err(std::io::Error::other)?;
        text.push('\n');
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(MANIFEST))?;
    storage::fsutil::sync_dir(dir).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(dir: &Path) {
        prepare(dir).unwrap();
        metadata::Metadata::open(&dir.join(METADATA_DB)).unwrap();
    }

    fn write_raw_manifest(dir: &Path, json: &str) {
        std::fs::write(dir.join(MANIFEST), json).unwrap();
    }

    fn layout(dir: &Path) -> Layout {
        check(dir, false).unwrap().layout
    }

    fn problems(dir: &Path) -> Vec<String> {
        match check(dir, false) {
            Err(DataDirError::Incomplete { problems, .. }) => problems,
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    #[test]
    fn new_directories() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(layout(&d.path().join("absent")), Layout::New);
        assert_eq!(layout(d.path()), Layout::New);
        // Entries that are not Vyrtel's do not make a directory used.
        std::fs::create_dir(d.path().join("lost+found")).unwrap();
        assert_eq!(layout(d.path()), Layout::New);
    }

    #[test]
    fn complete_directory_with_and_without_manifest() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        assert_eq!(layout(d.path()), Layout::Unversioned);
        record(d.path(), &Layout::Unversioned).unwrap();
        assert_eq!(layout(d.path()), Layout::Versioned(Manifest::current()));
        let text = std::fs::read_to_string(d.path().join(MANIFEST)).unwrap();
        assert!(text.contains("\"format\": 1"), "{text}");
        assert!(!d.path().join("vyrtel.json.tmp").exists());
        // Unknown manifest keys from the same format are ignored.
        write_raw_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.9", "note": "x"}"#);
        assert!(matches!(layout(d.path()), Layout::Versioned(m) if m.written_by == "0.1.9"));
    }

    #[test]
    fn interrupted_first_start_is_not_refused() {
        let d = tempfile::tempdir().unwrap();
        // Signal directories created, metadata database not yet.
        prepare(d.path()).unwrap();
        assert_eq!(layout(d.path()), Layout::Unversioned);
        // Database file created but not yet migrated, no telemetry yet.
        std::fs::create_dir_all(d.path().join("metadata")).unwrap();
        std::fs::write(d.path().join(METADATA_DB), b"").unwrap();
        assert_eq!(layout(d.path()), Layout::Unversioned);
        // Only the lock file.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".lock"), b"").unwrap();
        assert_eq!(layout(d.path()), Layout::Unversioned);
    }

    #[test]
    fn missing_parts_are_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        write_raw_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.1"}"#);
        std::fs::remove_dir(d.path().join("wal/traces")).unwrap();
        std::fs::remove_dir(d.path().join("segments/metrics")).unwrap();
        assert_eq!(problems(d.path()), ["wal/traces is missing", "segments/metrics is missing"]);
        let text = check(d.path(), false).unwrap_err().to_string();
        assert!(text.contains("is incomplete: wal/traces is missing; segments/metrics is missing. "), "{text}");

        // Telemetry without metadata, with or without a manifest.
        for manifest in [true, false] {
            let d = tempfile::tempdir().unwrap();
            prepare(d.path()).unwrap();
            std::fs::write(d.path().join("wal/logs/000000000001.wal"), b"").unwrap();
            if manifest {
                write_raw_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.1"}"#);
            }
            let text = check(d.path(), false).unwrap_err().to_string();
            assert!(
                text.ends_with(
                    " is incomplete: metadata/metadata.db is missing. Vyrtel did not start and changed nothing in \
                     it. If it is a restored backup, restore the whole data directory again; to start without the \
                     missing parts on purpose, start once with --accept-incomplete-restore (see \
                     docs/configuration.md#backup-and-restore)"
                ),
                "{text}"
            );
        }

        // Metadata without telemetry directories, without a manifest.
        let d = tempfile::tempdir().unwrap();
        metadata::Metadata::open(&d.path().join(METADATA_DB)).unwrap();
        assert_eq!(problems(d.path()).len(), 6);
    }

    #[test]
    fn an_empty_metadata_database_is_not_consent() {
        // With a manifest, or next to telemetry: a truncated or replaced
        // database, not a first start.
        for manifest in [true, false] {
            let d = tempfile::tempdir().unwrap();
            prepare(d.path()).unwrap();
            std::fs::write(d.path().join("segments/logs/000000000001.seg"), b"").unwrap();
            std::fs::create_dir_all(d.path().join("metadata")).unwrap();
            std::fs::write(d.path().join(METADATA_DB), b"").unwrap();
            if manifest {
                write_raw_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.1"}"#);
            }
            assert_eq!(problems(d.path()), ["metadata/metadata.db is empty (it has no schema)"], "{manifest}");
        }
    }

    #[test]
    fn the_inventory_is_checked_then_dropped() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        std::fs::write(d.path().join("wal/logs/000000000003.wal"), [0u8; 40]).unwrap();
        std::fs::write(d.path().join("segments/traces/000000000002.seg"), [0u8; 100]).unwrap();
        std::fs::write(d.path().join("segments/traces/notes.txt"), b"not telemetry").unwrap();
        record_stopped(d.path()).unwrap();
        let Layout::Versioned(m) = layout(d.path()) else { panic!("manifest written") };
        let files: Vec<(&str, u64)> = m.inventory.as_ref().unwrap().files.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(files, [("segments/traces/000000000002.seg", 100), ("wal/logs/000000000003.wal", 40)]);

        // One file left out of the restore, another truncated.
        std::fs::remove_file(d.path().join("wal/logs/000000000003.wal")).unwrap();
        std::fs::write(d.path().join("segments/traces/000000000002.seg"), [0u8; 60]).unwrap();
        assert_eq!(
            problems(d.path()),
            [
                "segments/traces/000000000002.seg has 60 bytes, 100 at the last clean shutdown",
                "wal/logs/000000000003.wal is missing"
            ]
        );
        // Extra files are not the inventory's business (recovery judges them).
        std::fs::write(d.path().join("wal/logs/000000000003.wal"), [0u8; 40]).unwrap();
        std::fs::write(d.path().join("segments/traces/000000000002.seg"), [0u8; 100]).unwrap();
        std::fs::write(d.path().join("wal/logs/000000000009.wal"), [0u8; 24]).unwrap();
        let checked = check(d.path(), false).unwrap();
        assert!(checked.accepted.is_empty());

        // Starting drops it before anything changes.
        begin(d.path(), &checked).unwrap();
        assert_eq!(layout(d.path()), Layout::Versioned(Manifest::current()));
        std::fs::remove_file(d.path().join("wal/logs/000000000003.wal")).unwrap();
        check(d.path(), false).unwrap();
    }

    #[test]
    fn accept_incomplete_returns_the_problems_instead() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        std::fs::write(d.path().join("wal/logs/000000000003.wal"), [0u8; 40]).unwrap();
        record_stopped(d.path()).unwrap();
        std::fs::remove_file(d.path().join("wal/logs/000000000003.wal")).unwrap();
        std::fs::remove_dir(d.path().join("wal/metrics")).unwrap();
        let checked = check(d.path(), true).unwrap();
        assert_eq!(checked.accepted, ["wal/metrics is missing", "wal/logs/000000000003.wal is missing"]);
        begin(d.path(), &checked).unwrap();
        // Format problems are never accepted.
        write_raw_manifest(d.path(), r#"{"format": 2, "writtenBy": "9.0.0"}"#);
        assert!(matches!(check(d.path(), true), Err(DataDirError::NewerFormat { .. })));
    }

    #[test]
    fn long_problem_lists_are_cut_short() {
        let items: Vec<String> = (0..13).map(|i| format!("p{i}")).collect();
        assert_eq!(list(&items), "p0; p1; p2; p3; p4; p5; p6; p7; p8; p9; and 3 more");
    }

    #[test]
    fn manifest_problems_are_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        write_raw_manifest(d.path(), r#"{"format": 2, "writtenBy": "0.3.0"}"#);
        let e = check(d.path(), false).unwrap_err();
        assert!(matches!(e, DataDirError::NewerFormat { found: 2, .. }), "{e:?}");
        let text = e.to_string();
        assert!(text.contains("has format 2 (written by Vyrtel 0.3.0)"), "{text}");
        assert!(text.contains("Start it with Vyrtel 0.3.0 or newer"), "{text}");

        for bad in [
            "",
            "{",
            r#"{"format": "one"}"#,
            r#"{"format": 0, "writtenBy": "x"}"#,
            r#"{"writtenBy": "x"}"#,
            r#"{"format": 1, "writtenBy": "x", "inventory": {"files": {"../outside": 1}}}"#,
            r#"{"format": 1, "writtenBy": "x", "inventory": {"files": {"metadata/metadata.db": 1}}}"#,
        ] {
            write_raw_manifest(d.path(), bad);
            assert!(matches!(check(d.path(), false), Err(DataDirError::BadManifest { .. })), "{bad:?}");
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
        let e = check(d.path(), false).unwrap_err();
        assert!(matches!(e, DataDirError::NewerMetadata { found: 99, .. }), "{e:?}");
        assert!(e.to_string().contains("schema version 99"), "{e}");

        std::fs::write(d.path().join(METADATA_DB), b"definitely not sqlite, but long enough to look like a page")
            .unwrap();
        assert!(matches!(check(d.path(), false), Err(DataDirError::BadMetadata { .. })));
    }
}
