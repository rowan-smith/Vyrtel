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
//!
//! Upgrades (REL-07): [`plan`] lists what a start would migrate in a
//! directory that passed the check, and [`RELEASES`] which released
//! versions can read it before and after, so `vyrtel check-data-dir` can
//! tell an operator, before upgrading and without changing anything,
//! whether this build reads their data and what going back would take.

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
/// 1, WAL record version 1, event codec 2 (WAL batches with a resource
/// table; codec 1 is still read), segment format 1, metadata schema up to
/// [`metadata::SCHEMA_VERSION`]. Format 1 (0.1.1) differs only in the event
/// codec, so a format 1 directory is read as it is and new WAL records are
/// written in codec 2.
pub const FORMAT: u32 = 2;

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
    /// The metadata schema version found: 0 without a database, or for an
    /// empty one.
    pub schema: i64,
}

/// A released Vyrtel version and the newest data-directory format and
/// metadata schema it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    pub version: &'static str,
    pub format: u32,
    pub schema: i64,
}

/// Every Vyrtel release, oldest first. Add a row when cutting a release
/// (see docs/releasing.md). 0.1.1 does not check formats: it would misread
/// a directory in a newer format instead of refusing it. 0.1.0 was released
/// as Observer, whose files Vyrtel does not read and which does not read
/// Vyrtel's, so it is not listed.
pub const RELEASES: &[Release] = &[Release { version: "0.1.1", format: 1, schema: 1 }];

/// The oldest release that reads a directory in `format` with metadata
/// `schema`; `None` if no release does (a format not released yet).
pub fn readable_since(format: u32, schema: i64) -> Option<&'static str> {
    RELEASES.iter().find(|r| r.format >= format && r.schema >= schema).map(|r| r.version)
}

/// One change a start makes to the formats in a data directory that
/// passed [`check`], in the order it makes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A new directory is created in the current format.
    Create,
    /// A directory from before manifests gets one.
    RecordFormat,
    /// A directory in an older format is recorded in this one. Its files
    /// are read as they are; what is written from then on is in this format.
    UpgradeFormat { from: u32 },
    /// The metadata database is migrated, one transaction per version.
    MigrateSchema { from: i64, to: i64 },
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::Create => write!(f, "create a new data directory in format {FORMAT}"),
            Step::RecordFormat => {
                write!(f, "record data-directory format {FORMAT} in {MANIFEST} (it has none: written before 0.1.2)")
            }
            Step::UpgradeFormat { from } => write!(
                f,
                "record data-directory format {FORMAT} in {MANIFEST} (it has {from}); files already written are read \
                 as they are, and WAL batches are written in event codec 2 from then on"
            ),
            Step::MigrateSchema { from: 0, to } => write!(f, "create the schema of {METADATA_DB} (version {to})"),
            Step::MigrateSchema { from, to } => write!(f, "migrate {METADATA_DB} from schema version {from} to {to}"),
        }
    }
}

/// What starting a directory that passed [`check`] changes in its formats.
/// Every step is safe to interrupt: the manifest is replaced atomically and
/// each schema migration is one SQLite transaction, so a start after an
/// interruption redoes what was left. Telemetry files are not rewritten.
pub fn plan(checked: &Checked) -> Vec<Step> {
    let mut steps = Vec::new();
    match &checked.layout {
        Layout::New => return vec![Step::Create],
        Layout::Unversioned => steps.push(Step::RecordFormat),
        Layout::Versioned(m) if m.format < FORMAT => steps.push(Step::UpgradeFormat { from: m.format }),
        Layout::Versioned(_) => {}
    }
    if checked.schema < metadata::SCHEMA_VERSION {
        steps.push(Step::MigrateSchema { from: checked.schema, to: metadata::SCHEMA_VERSION });
    }
    steps
}

/// The `vyrtel check-data-dir` report for a directory that passed [`check`]:
/// what it holds, what starting this build would change, and which
/// releases can read it before and after.
pub fn report(dir: &Path, checked: &Checked) -> String {
    let this = env!("CARGO_PKG_VERSION");
    let schema = metadata::SCHEMA_VERSION;
    let mut out = format!("data directory {}\n", dir.display());
    let format = match &checked.layout {
        Layout::New => None,
        Layout::Unversioned => {
            out.push_str(&format!("  {MANIFEST}: none (written before 0.1.2, or a first start that stopped early)\n"));
            Some(1)
        }
        Layout::Versioned(m) => {
            out.push_str(&format!("  {MANIFEST}: format {}, last written by Vyrtel {}\n", m.format, m.written_by));
            out.push_str(match m.inventory {
                Some(_) => "  clean-shutdown inventory: every WAL and segment file is present with its size\n",
                None => "  clean-shutdown inventory: none, so missing WAL or segment files cannot be detected\n",
            });
            Some(m.format)
        }
    };
    match (format, checked.schema) {
        (None, _) => out.push_str("  new: none of Vyrtel's files yet\n"),
        (Some(_), 0) => out.push_str(&format!("  {METADATA_DB}: no schema yet\n")),
        (Some(_), v) => out.push_str(&format!("  {METADATA_DB}: schema version {v}\n")),
    }
    for problem in &checked.accepted {
        out.push_str(&format!("  accepted with --accept-incomplete-restore: {problem}\n"));
    }

    out.push_str(&format!("Vyrtel {this} (format {FORMAT}, metadata schema up to {schema}) can start it.\n"));
    let steps = plan(checked);
    let Some(format) = format else {
        out.push_str(&format!("Starting it will {}.\n", Step::Create));
        return out;
    };
    if steps.is_empty() {
        out.push_str("Starting it migrates nothing.\n");
    } else {
        out.push_str("Starting it will:\n");
        for step in &steps {
            out.push_str(&format!("  - {step}\n"));
        }
    }
    let (before, after) = (readable_since(format, checked.schema), readable_since(FORMAT, schema));
    match (before, after) {
        (_, None) => out.push_str(
            "Afterwards no released Vyrtel version can read it, only this build and later ones. To go back, \
             restore the backup taken before the upgrade.\n",
        ),
        (Some(before), Some(after)) if before == after => out.push_str(&format!(
            "Afterwards every release that reads it now (Vyrtel {after} and later) can still read it.\n"
        )),
        (_, Some(after)) => out.push_str(&format!(
            "Afterwards only Vyrtel {after} and later can read it. To go back to an older version, restore the \
             backup taken before the upgrade.\n"
        )),
    }
    if !steps.is_empty() && before != after {
        out.push_str(
            "Back up the stopped data directory first (see docs/configuration.md#upgrades-and-data-migrations).\n",
        );
    }
    out
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
    /// Observer 0.1.0 (Vyrtel's first release, under its earlier name)
    /// wrote the telemetry files, in a format no Vyrtel release reads.
    Observer { dir: PathBuf, file: String },
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
                 version reads versions up to {supported}. {NOTHING_CHANGED}. Start it with the Vyrtel version that wrote \
                 it, or newer ({SEE})",
                dir.display()
            ),
            DataDirError::Observer { dir, file } => write!(
                f,
                "data directory {} was written by Observer 0.1.0, Vyrtel's first release under its earlier name \
                 ({file} is in Observer's file format), and Vyrtel {this} cannot read its telemetry. \
                 {NOTHING_CHANGED}. Keep running Observer 0.1.0 on it, or start Vyrtel on a new data directory \
                 (see docs/configuration.md#upgrades-and-data-migrations)",
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
    let new = Checked { layout: Layout::New, accepted: Vec::new(), schema: 0 };
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
        StorageError::ObserverFormat { path } => {
            DataDirError::Observer { dir: dir.to_path_buf(), file: relative(dir, &path) }
        }
        e => DataDirError::Unreadable { dir: dir.to_path_buf(), reason: e.to_string() },
    })?;

    let db = dir.join(METADATA_DB);
    let mut schema = 0;
    if db.exists() {
        schema = match metadata::Metadata::inspect(&db) {
            Ok(found) if found > metadata::SCHEMA_VERSION => {
                return Err(DataDirError::NewerMetadata { dir: dir.to_path_buf(), found });
            }
            // Every start migrates the database before it writes telemetry
            // or a manifest, so an empty one next to either was truncated or
            // replaced: its dashboards, alerts, users and keys are gone.
            Ok(0) if matches!(layout, Layout::Versioned(_)) || has_telemetry(dir).map_err(unreadable)? => {
                problems.push(format!("{METADATA_DB} is empty (it has no schema)"));
                0
            }
            Ok(found) => found,
            Err(e) => return Err(DataDirError::BadMetadata { dir: dir.to_path_buf(), reason: e.to_string() }),
        };
    }

    if !problems.is_empty() && !accept_incomplete {
        return Err(DataDirError::Incomplete { dir: dir.to_path_buf(), problems });
    }
    Ok(Checked { layout, accepted: problems, schema })
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
    storage::fault::check("manifest.write", &tmp)?;
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
        assert!(text.contains(&format!("\"format\": {FORMAT}")), "{text}");
        assert!(!d.path().join("vyrtel.json.tmp").exists());
        // Unknown manifest keys from the same format are ignored.
        write_raw_manifest(d.path(), &format!(r#"{{"format": {FORMAT}, "writtenBy": "0.1.9", "note": "x"}}"#));
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
        let files: Vec<(&str, u64)> =
            m.inventory.as_ref().unwrap().files.iter().map(|(k, v)| (k.as_str(), *v)).collect();
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
        write_raw_manifest(d.path(), r#"{"format": 3, "writtenBy": "9.0.0"}"#);
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
        write_raw_manifest(d.path(), r#"{"format": 3, "writtenBy": "0.3.0"}"#);
        let e = check(d.path(), false).unwrap_err();
        assert!(matches!(e, DataDirError::NewerFormat { found: 3, .. }), "{e:?}");
        let text = e.to_string();
        assert!(text.contains("has format 3 (written by Vyrtel 0.3.0)"), "{text}");
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

    #[test]
    fn releases_are_listed_oldest_first_and_never_shrink() {
        let key = |v: &str| v.split('.').map(|n| n.parse::<u64>().unwrap()).collect::<Vec<_>>();
        for pair in RELEASES.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(key(a.version) < key(b.version), "{a:?} before {b:?}");
            assert!(a.format <= b.format && a.schema <= b.schema, "{a:?} reads more than {b:?}");
        }
        let last = RELEASES.last().unwrap();
        assert!(last.format <= FORMAT && last.schema <= metadata::SCHEMA_VERSION, "{last:?}");
        // The version bump of a release adds its row (docs/releasing.md), and
        // the row says what that release reads. Between releases the build
        // still carries the last release's version but may already read a
        // newer format, so the row may claim less than the build, never more.
        // Prereleases need none.
        let this = env!("CARGO_PKG_VERSION");
        if !this.contains('-') {
            let r = RELEASES.iter().find(|r| r.version == this);
            let r = r.unwrap_or_else(|| panic!("add Vyrtel {this} to datadir::RELEASES (see docs/releasing.md)"));
            assert!(r.format <= FORMAT && r.schema <= metadata::SCHEMA_VERSION, "RELEASES row for {this}: {r:?}");
        }
        assert_eq!(readable_since(1, 1), Some("0.1.1"));
        assert_eq!(readable_since(FORMAT + 1, 1), None);
        assert_eq!(readable_since(1, metadata::SCHEMA_VERSION + 1), None);
    }

    fn checked(dir: &Path) -> Checked {
        check(dir, false).unwrap()
    }

    #[test]
    fn plans_name_every_migration_a_start_makes() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(plan(&checked(d.path())), [Step::Create]);

        // A first start interrupted before the database had a schema.
        prepare(d.path()).unwrap();
        let c = checked(d.path());
        assert_eq!((c.layout.clone(), c.schema), (Layout::Unversioned, 0));
        assert_eq!(plan(&c), [Step::RecordFormat, Step::MigrateSchema { from: 0, to: metadata::SCHEMA_VERSION }]);

        // A directory from 0.1.1: only the manifest is new.
        complete(d.path());
        let c = checked(d.path());
        assert_eq!(c.schema, metadata::SCHEMA_VERSION);
        assert_eq!(plan(&c), [Step::RecordFormat]);
        assert_eq!(
            Step::RecordFormat.to_string(),
            "record data-directory format 2 in vyrtel.json (it has none: written before 0.1.2)"
        );

        // A directory in format 1 (0.1.1 with a manifest, or an earlier build
        // of 0.1.2): recorded in format 2, its files read as they are.
        write_raw_manifest(d.path(), r#"{"format": 1, "writtenBy": "0.1.1"}"#);
        assert_eq!(plan(&checked(d.path())), [Step::UpgradeFormat { from: 1 }]);
        assert_eq!(
            Step::UpgradeFormat { from: 1 }.to_string(),
            "record data-directory format 2 in vyrtel.json (it has 1); files already written are read as they \
             are, and WAL batches are written in event codec 2 from then on"
        );

        // A current directory: nothing to do.
        record(d.path(), &Layout::Unversioned).unwrap();
        assert!(plan(&checked(d.path())).is_empty());

        // An older schema (not reachable with one migration, so built by hand).
        let older = Checked { schema: 1, ..checked(d.path()) };
        let step = Step::MigrateSchema { from: 1, to: 2 };
        assert_eq!(step.to_string(), "migrate metadata/metadata.db from schema version 1 to 2");
        assert_eq!(plan(&older).len(), usize::from(metadata::SCHEMA_VERSION > 1));
    }

    #[test]
    fn the_report_says_what_changes_and_who_can_read_it_afterwards() {
        let this = env!("CARGO_PKG_VERSION");
        let d = tempfile::tempdir().unwrap();
        let text = report(d.path(), &checked(d.path()));
        assert!(text.contains("  new: none of Vyrtel's files yet\n"), "{text}");
        assert!(
            text.ends_with(&format!(
                "Vyrtel {this} (format 2, metadata schema up to 1) can start it.\nStarting it will create a new data \
             directory in format 2.\n"
            )),
            "{text}"
        );

        complete(d.path());
        let text = report(d.path(), &checked(d.path()));
        let expected = format!(
            "  vyrtel.json: none (written before 0.1.2, or a first start that stopped early)\n  \
             metadata/metadata.db: schema version 1\nVyrtel {this} (format 2, metadata schema up to 1) can start \
             it.\nStarting it will:\n  - record data-directory format 2 in vyrtel.json (it has none: written \
             before 0.1.2)\nAfterwards no released Vyrtel version can read it, only this build and later ones. To \
             go back, restore the backup taken before the upgrade.\nBack up the stopped data directory first (see \
             docs/configuration.md#upgrades-and-data-migrations).\n"
        );
        assert!(text.ends_with(&expected), "{text}");

        std::fs::write(d.path().join("wal/logs/000000000003.wal"), [0u8; 40]).unwrap();
        record_stopped(d.path()).unwrap();
        let text = report(d.path(), &checked(d.path()));
        assert!(text.contains(&format!("  vyrtel.json: format 2, last written by Vyrtel {this}\n")), "{text}");
        assert!(text.contains("  clean-shutdown inventory: every WAL and segment file is present with its size\n"));
        assert!(text.contains("Starting it migrates nothing.\n"), "{text}");

        std::fs::remove_file(d.path().join("wal/logs/000000000003.wal")).unwrap();
        let c = check(d.path(), true).unwrap();
        let text = report(d.path(), &c);
        assert!(
            text.contains("  accepted with --accept-incomplete-restore: wal/logs/000000000003.wal is missing\n"),
            "{text}"
        );

        // A schema migration asks for a backup first.
        let d = tempfile::tempdir().unwrap();
        prepare(d.path()).unwrap();
        let text = report(d.path(), &checked(d.path()));
        assert!(text.contains("  metadata/metadata.db: no schema yet\n"), "{text}");
        assert!(text.contains("  - create the schema of metadata/metadata.db (version 1)\n"), "{text}");
        assert!(text.ends_with(
            "Back up the stopped data directory first (see docs/configuration.md#upgrades-and-data-migrations).\n"
        ));
    }

    #[test]
    fn observer_data_is_refused() {
        let d = tempfile::tempdir().unwrap();
        prepare(d.path()).unwrap();
        std::fs::create_dir_all(d.path().join("metadata")).unwrap();
        drop(metadata::Metadata::open(&d.path().join(METADATA_DB)).unwrap());
        let mut header = storage::wal::OBSERVER_WAL_MAGIC.to_vec();
        header.resize(storage::wal::HEADER_LEN as usize, 0);
        std::fs::write(d.path().join("wal/traces/000000000001.wal"), header).unwrap();
        let e = check(d.path(), true).unwrap_err();
        assert!(matches!(&e, DataDirError::Observer { file, .. } if file == "wal/traces/000000000001.wal"), "{e:?}");
        let text = e.to_string();
        assert!(
            text.contains(" was written by Observer 0.1.0, Vyrtel's first release under its earlier name"),
            "{text}"
        );
        assert!(text.contains("Keep running Observer 0.1.0 on it, or start Vyrtel on a new data directory"), "{text}");
    }
}
