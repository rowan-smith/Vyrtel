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
//! A complete directory carries a manifest, [`MANIFEST`], with the layout
//! identity ([`telemetry::LAYOUT`]) and the data-directory [`FORMAT`]. Every
//! file in it carries the identity too: each WAL and segment header and the
//! metadata database. Before 1.0 a release reads only directories of its
//! own layout: one written by another release, or by any release before
//! 0.1.2 (which wrote no identity), is refused before a single data file is
//! read, never migrated or guessed at (COMPAT-GATE-0.1.2; see
//! docs/configuration.md#upgrading-between-pre-10-releases).
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
//! [`report`] tells an operator, before starting and without changing
//! anything, whether this build reads a directory (`vyrtel check-data-dir`).

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use metadata::Identity;
use serde::{Deserialize, Serialize};
use storage::StorageError;
use telemetry::{LAYOUT, Signal};

/// Manifest file name, at the top of the data directory.
pub const MANIFEST: &str = "vyrtel.json";

/// The data-directory format of layout [`LAYOUT`]: WAL file format 1, WAL
/// record version 1, event codec 1, segment format 1 and metadata schema
/// [`metadata::SCHEMA_VERSION`] (1). The only format of the layout.
pub const FORMAT: u32 = 1;

/// Where the metadata database lives, relative to the data directory.
pub const METADATA_DB: &str = "metadata/metadata.db";

/// Names Vyrtel creates at the top of a data directory. A directory with
/// none of them is new (it may hold unrelated entries such as `lost+found`).
const OWN_ENTRIES: &[&str] = &[MANIFEST, ".lock", "metadata", "wal", "segments", "quarantine", "indexes", "tmp"];

/// How many problems a [`DataDirError::Incomplete`] message lists.
const LISTED: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    /// The layout identity: [`LAYOUT`] for every directory this build
    /// reads.
    pub layout: String,
    /// The Vyrtel version that last wrote the manifest.
    pub written_by: String,
    /// Present only while the directory is stopped after a clean shutdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Inventory>,
}

/// The telemetry files of a cleanly stopped data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    /// Path relative to the data directory (`wal/logs/000000000003.wal`)
    /// → size in bytes.
    pub files: BTreeMap<String, u64>,
}

impl Manifest {
    pub fn current() -> Manifest {
        Manifest {
            format: FORMAT,
            layout: LAYOUT.to_string(),
            written_by: env!("CARGO_PKG_VERSION").to_string(),
            inventory: None,
        }
    }
}

/// What [`check`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// No data directory yet, or none of Vyrtel's entries in it.
    New,
    /// No manifest yet, and nothing of another layout: a first start of
    /// this build that stopped before writing one (no telemetry, and a
    /// metadata database that is absent, empty or of this layout). Gets
    /// one. Also a directory whose only missing part is the manifest,
    /// accepted with `accept_incomplete`.
    Unrecorded,
    /// A directory with a manifest of this layout.
    Current(Manifest),
}

/// The result of a successful [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub layout: Layout,
    /// Problems that would have refused the start, accepted on request
    /// (`accept_incomplete`). Empty otherwise.
    pub accepted: Vec<String>,
    /// The metadata database found: `None` without one, else
    /// [`Identity::Empty`] or [`Identity::Current`] (anything else is
    /// refused).
    pub metadata: Option<Identity>,
}

/// The `vyrtel check-data-dir` report for a directory that passed [`check`]:
/// what it holds and what starting this build would change.
pub fn report(dir: &Path, checked: &Checked) -> String {
    let this = env!("CARGO_PKG_VERSION");
    let schema = metadata::SCHEMA_VERSION;
    let mut out = format!("data directory {}\n", dir.display());
    match &checked.layout {
        Layout::New => out.push_str("  new: none of Vyrtel's files yet\n"),
        Layout::Unrecorded => {
            out.push_str(&format!("  {MANIFEST}: none (a first start of this layout stopped before writing it)\n"))
        }
        Layout::Current(m) => {
            out.push_str(&format!(
                "  {MANIFEST}: layout {}, format {}, last written by Vyrtel {}\n",
                m.layout, m.format, m.written_by
            ));
            out.push_str(match m.inventory {
                Some(_) => "  clean-shutdown inventory: every WAL and segment file is present with its size\n",
                None => "  clean-shutdown inventory: none, so missing WAL or segment files cannot be detected\n",
            });
        }
    }
    if checked.layout != Layout::New {
        out.push_str(&match checked.metadata {
            None => format!("  {METADATA_DB}: none\n"),
            Some(Identity::Empty) => format!("  {METADATA_DB}: no schema yet\n"),
            Some(_) => format!("  {METADATA_DB}: layout {LAYOUT}, schema version {schema}\n"),
        });
    }
    for problem in &checked.accepted {
        out.push_str(&format!("  accepted with --accept-incomplete-restore: {problem}\n"));
    }

    out.push_str(&format!(
        "Vyrtel {this} (layout {LAYOUT}, data-directory format {FORMAT}, metadata schema {schema}) can start it.\n"
    ));
    out.push_str(match checked.layout {
        Layout::New => "Starting it will create a new data directory in this layout.\n",
        Layout::Unrecorded => "Starting it will finish setting it up and record the layout in vyrtel.json.\n",
        Layout::Current(_) => "Starting it changes no file formats.\n",
    });
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
    /// The directory, or a file in it, is not of this build's layout:
    /// another layout identity, or none (written before 0.1.2). `what`
    /// names the file and what it has instead.
    ForeignLayout { dir: PathBuf, what: String },
    /// A file of this layout declares a format version this build does not
    /// read (a newer build of the layout, or edited by hand).
    UnsupportedFile { dir: PathBuf, file: String, what: &'static str, found: u32, supported: u32 },
    /// The metadata database cannot be read (not SQLite, damaged).
    BadMetadata { dir: PathBuf, reason: String },
    /// The directory could not be examined (permissions, I/O errors).
    Unreadable { dir: PathBuf, reason: String },
}

const NOTHING_CHANGED: &str = "Vyrtel did not start and changed nothing in it";
const SEE: &str = "see docs/configuration.md#backup-and-restore";
const SEE_LAYOUTS: &str = "see docs/configuration.md#upgrading-between-pre-10-releases";

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
            DataDirError::ForeignLayout { dir, what } => write!(
                f,
                "data directory {} is not in the layout Vyrtel {this} reads: {what}. Vyrtel {this} reads layout \
                 {LAYOUT} only, and before 1.0 a release reads no data directory of another layout. \
                 {NOTHING_CHANGED}. Keep running the Vyrtel version that wrote it, or start Vyrtel {this} on a \
                 new, empty data directory ({SEE_LAYOUTS})",
                dir.display()
            ),
            DataDirError::UnsupportedFile { dir, file, what, found, supported } => write!(
                f,
                "data directory {} cannot be read by Vyrtel {this}: {file} uses {what} version {found}, and this \
                 version reads version {supported} of layout {LAYOUT} only. {NOTHING_CHANGED}. Start it with the \
                 Vyrtel build that wrote it ({SEE_LAYOUTS})",
                dir.display()
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
/// Always refused: an unreadable manifest; a manifest, metadata database,
/// WAL or segment file of another layout or without a layout identity;
/// WAL or segment files with neither a manifest nor a metadata database of
/// this layout to identify them; a file of this layout in a format version
/// this build does not read; a metadata database that cannot be read.
///
/// Refused as incomplete, unless `accept_incomplete` (then the problems are
/// returned in [`Checked::accepted`]): a missing required entry, a file in
/// the inventory that is missing or has another size, a metadata database
/// without a schema where one must exist, and a missing manifest next to
/// telemetry and a metadata database of this layout. Damaged segments and
/// WAL tails are not refused; recovery quarantines them as after a crash.
pub fn check(dir: &Path, accept_incomplete: bool) -> Result<Checked, DataDirError> {
    let unreadable = |e: std::io::Error| DataDirError::Unreadable { dir: dir.to_path_buf(), reason: e.to_string() };
    let foreign = |what: String| DataDirError::ForeignLayout { dir: dir.to_path_buf(), what };
    let new = Checked { layout: Layout::New, accepted: Vec::new(), metadata: None };
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

    let manifest = match std::fs::read(dir.join(MANIFEST)) {
        Ok(bytes) => Some(read_manifest(dir, &bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(unreadable(e)),
    };
    let db = dir.join(METADATA_DB);
    let metadata = if db.exists() {
        match metadata::Metadata::inspect(&db) {
            Ok(Identity::Foreign(found)) if manifest.is_none() => {
                return Err(foreign(format!("there is no {MANIFEST}, and {METADATA_DB} has {found}")));
            }
            Ok(Identity::Foreign(found)) => return Err(foreign(format!("{METADATA_DB} has {found}"))),
            Ok(identity) => Some(identity),
            Err(e) => return Err(DataDirError::BadMetadata { dir: dir.to_path_buf(), reason: e.to_string() }),
        }
    } else {
        None
    };
    let telemetry = has_telemetry(dir).map_err(unreadable)?;

    let present = |rel: &str| dir.join(rel).exists();
    let missing = |rel: String| format!("{rel} is missing");
    let mut problems = Vec::new();
    let layout = match manifest {
        Some(m) => {
            // A manifest is written only once every required entry exists.
            problems.extend(required().into_iter().filter(|r| !present(r)).map(missing));
            for (rel, &expected) in m.inventory.iter().flat_map(|i| &i.files) {
                match std::fs::metadata(dir.join(rel)) {
                    Ok(f) if f.len() == expected => {}
                    Ok(f) => {
                        problems.push(format!("{rel} has {} bytes, {expected} at the last clean shutdown", f.len()))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => problems.push(missing(rel.clone())),
                    Err(e) => return Err(unreadable(e)),
                }
            }
            // Every start creates the database's schema before it writes a
            // manifest, so an empty one next to it was truncated or
            // replaced: its dashboards, alerts, users and keys are gone.
            if metadata == Some(Identity::Empty) {
                problems.push(format!("{METADATA_DB} is empty (it has no schema)"));
            }
            Layout::Current(m)
        }
        None => {
            // A first start records the manifest before it writes any
            // telemetry, so telemetry without one is identified only by a
            // metadata database of this layout (a restore that left out the
            // manifest). Without either, it is from before 0.1.2, or cannot
            // be told apart from it: never guessed at.
            let current_db = metadata == Some(Identity::Current);
            if telemetry && !current_db {
                return Err(foreign(format!(
                    "there is no {MANIFEST} and no metadata database of this layout, but there are WAL or segment \
                     files (Vyrtel 0.1.1 and earlier wrote no {MANIFEST})"
                )));
            }
            if telemetry {
                problems.push(missing(MANIFEST.to_string()));
            }
            // A first start creates the signal directories, then the
            // database, so one interrupted in between is not refused.
            if metadata.is_some() {
                problems.extend(required().into_iter().skip(1).filter(|r| !present(r)).map(missing));
            }
            Layout::Unrecorded
        }
    };

    storage::check_formats(dir).map_err(|e| match e {
        StorageError::ForeignLayout { path, what, found } => {
            foreign(format!("{} is a {what} file with {found}", relative(dir, &path)))
        }
        StorageError::UnsupportedFormat { path, what, found, supported } => DataDirError::UnsupportedFile {
            dir: dir.to_path_buf(),
            file: relative(dir, &path),
            what,
            found: found.into(),
            supported: supported.into(),
        },
        e => DataDirError::Unreadable { dir: dir.to_path_buf(), reason: e.to_string() },
    })?;

    if !problems.is_empty() && !accept_incomplete {
        return Err(DataDirError::Incomplete { dir: dir.to_path_buf(), problems });
    }
    Ok(Checked { layout, accepted: problems, metadata })
}

/// The manifest's layout identity first: only a manifest of this layout is
/// read any further.
fn read_manifest(dir: &Path, bytes: &[u8]) -> Result<Manifest, DataDirError> {
    let bad = |reason: String| DataDirError::BadManifest { dir: dir.to_path_buf(), reason };
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("not a valid manifest ({e})")))?;
    let Some(fields) = value.as_object() else { return Err(bad("not a valid manifest (not an object)".into())) };
    let written_by = match fields.get("writtenBy").and_then(|w| w.as_str()) {
        Some(w) => format!(", written by Vyrtel {w}"),
        None => String::new(),
    };
    match fields.get("layout") {
        Some(serde_json::Value::String(layout)) if layout == LAYOUT => {}
        Some(serde_json::Value::String(layout)) => {
            return Err(DataDirError::ForeignLayout {
                dir: dir.to_path_buf(),
                what: format!("{MANIFEST} has layout {layout}{written_by}"),
            });
        }
        Some(_) => return Err(bad("its layout is not a string".into())),
        // Manifests of 0.1.2 builds before the layout reset have a format
        // but no layout.
        None if fields.contains_key("format") => {
            let format = fields.get("format").map(|f| f.to_string()).unwrap_or_default();
            return Err(DataDirError::ForeignLayout {
                dir: dir.to_path_buf(),
                what: format!("{MANIFEST} has no layout identity (format {format}{written_by})"),
            });
        }
        None => return Err(bad("it has no layout".into())),
    }
    let m: Manifest = serde_json::from_value(value).map_err(|e| bad(format!("not a valid manifest ({e})")))?;
    if m.format != FORMAT {
        return Err(DataDirError::UnsupportedFile {
            dir: dir.to_path_buf(),
            file: MANIFEST.into(),
            what: "data-directory format",
            found: m.format,
            supported: FORMAT,
        });
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
    if let Layout::Current(m) = &checked.layout
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
        Layout::New | Layout::Unrecorded => {
            tracing::info!(layout = LAYOUT, format = FORMAT, "initialised data directory")
        }
        Layout::Current(_) => {}
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

    /// A manifest of this layout, as JSON, with `extra` keys appended.
    fn manifest_json(extra: &str) -> String {
        format!(r#"{{"format": 1, "layout": "{LAYOUT}", "writtenBy": "0.1.2"{extra}}}"#)
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

    fn foreign(dir: &Path) -> String {
        match check(dir, true) {
            Err(e @ DataDirError::ForeignLayout { .. }) => {
                let DataDirError::ForeignLayout { what, .. } = &e else { unreachable!() };
                let text = e.to_string();
                assert!(
                    text.ends_with(&format!(
                        ". Vyrtel {this} reads layout {LAYOUT} only, and before 1.0 a release reads no data directory \
                         of another layout. Vyrtel did not start and changed nothing in it. Keep running the Vyrtel \
                         version that wrote it, or start Vyrtel {this} on a new, empty data directory (see \
                         docs/configuration.md#upgrading-between-pre-10-releases)",
                        this = env!("CARGO_PKG_VERSION")
                    )),
                    "{text}"
                );
                what.clone()
            }
            other => panic!("expected ForeignLayout (never accepted), got {other:?}"),
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
        assert_eq!(layout(d.path()), Layout::Unrecorded);
        record(d.path(), &Layout::Unrecorded).unwrap();
        assert_eq!(layout(d.path()), Layout::Current(Manifest::current()));
        let text = std::fs::read_to_string(d.path().join(MANIFEST)).unwrap();
        assert!(text.contains(&format!("\"layout\": \"{LAYOUT}\"")), "{text}");
        assert!(text.contains("\"format\": 1"), "{text}");
        assert!(!d.path().join("vyrtel.json.tmp").exists());
        write_raw_manifest(d.path(), &manifest_json(""));
        assert!(matches!(layout(d.path()), Layout::Current(m) if m.written_by == "0.1.2"));
    }

    #[test]
    fn interrupted_first_start_is_not_refused() {
        let d = tempfile::tempdir().unwrap();
        // Signal directories created, metadata database not yet.
        prepare(d.path()).unwrap();
        assert_eq!(layout(d.path()), Layout::Unrecorded);
        // Database file created but its schema not yet committed.
        std::fs::create_dir_all(d.path().join("metadata")).unwrap();
        std::fs::write(d.path().join(METADATA_DB), b"").unwrap();
        assert_eq!(layout(d.path()), Layout::Unrecorded);
        // Schema committed, manifest not yet written.
        drop(metadata::Metadata::open(&d.path().join(METADATA_DB)).unwrap());
        assert_eq!(layout(d.path()), Layout::Unrecorded);
        // Only the lock file.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".lock"), b"").unwrap();
        assert_eq!(layout(d.path()), Layout::Unrecorded);
    }

    #[test]
    fn missing_parts_are_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        write_raw_manifest(d.path(), &manifest_json(""));
        std::fs::remove_dir(d.path().join("wal/traces")).unwrap();
        std::fs::remove_dir(d.path().join("segments/metrics")).unwrap();
        assert_eq!(problems(d.path()), ["wal/traces is missing", "segments/metrics is missing"]);
        let text = check(d.path(), false).unwrap_err().to_string();
        assert!(text.contains("is incomplete: wal/traces is missing; segments/metrics is missing. "), "{text}");

        // Telemetry without metadata, with a manifest.
        let d = tempfile::tempdir().unwrap();
        prepare(d.path()).unwrap();
        std::fs::write(d.path().join("wal/logs/000000000001.wal"), b"").unwrap();
        write_raw_manifest(d.path(), &manifest_json(""));
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

        // Metadata without telemetry directories, without a manifest.
        let d = tempfile::tempdir().unwrap();
        metadata::Metadata::open(&d.path().join(METADATA_DB)).unwrap();
        assert_eq!(problems(d.path()).len(), 6);

        // Telemetry and metadata of this layout, but no manifest: a restore
        // that left it out.
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        std::fs::write(d.path().join("segments/logs/000000000001.seg"), b"").unwrap();
        assert_eq!(problems(d.path()), ["vyrtel.json is missing"]);
        assert_eq!(check(d.path(), true).unwrap().layout, Layout::Unrecorded);
    }

    #[test]
    fn an_empty_metadata_database_is_not_consent() {
        // With a manifest: a truncated or replaced database, not a first start.
        let d = tempfile::tempdir().unwrap();
        prepare(d.path()).unwrap();
        std::fs::write(d.path().join("segments/logs/000000000001.seg"), b"").unwrap();
        std::fs::create_dir_all(d.path().join("metadata")).unwrap();
        std::fs::write(d.path().join(METADATA_DB), b"").unwrap();
        write_raw_manifest(d.path(), &manifest_json(""));
        assert_eq!(problems(d.path()), ["metadata/metadata.db is empty (it has no schema)"]);
    }

    /// FR-AC3: telemetry that neither a manifest nor a metadata database of
    /// this layout identifies is refused, never accepted as incomplete.
    #[test]
    fn unidentified_telemetry_is_refused() {
        for db in [false, true] {
            let d = tempfile::tempdir().unwrap();
            prepare(d.path()).unwrap();
            std::fs::write(d.path().join("wal/logs/000000000001.wal"), b"").unwrap();
            if db {
                std::fs::create_dir_all(d.path().join("metadata")).unwrap();
                std::fs::write(d.path().join(METADATA_DB), b"").unwrap();
            }
            assert_eq!(
                foreign(d.path()),
                "there is no vyrtel.json and no metadata database of this layout, but there are WAL or segment \
                 files (Vyrtel 0.1.1 and earlier wrote no vyrtel.json)"
            );
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
        let Layout::Current(m) = layout(d.path()) else { panic!("manifest written") };
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
        assert_eq!(layout(d.path()), Layout::Current(Manifest::current()));
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
        // Layout problems are never accepted.
        write_raw_manifest(d.path(), r#"{"format": 1, "layout": "9.0.0", "writtenBy": "9.0.0"}"#);
        assert_eq!(foreign(d.path()), "vyrtel.json has layout 9.0.0, written by Vyrtel 9.0.0");
    }

    #[test]
    fn long_problem_lists_are_cut_short() {
        let items: Vec<String> = (0..13).map(|i| format!("p{i}")).collect();
        assert_eq!(list(&items), "p0; p1; p2; p3; p4; p5; p6; p7; p8; p9; and 3 more");
    }

    /// FR-AC3: the manifest's identity decides before anything else in it,
    /// and a version number alone identifies nothing.
    #[test]
    fn manifests_of_other_layouts_are_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        // 0.1.2 builds before the layout reset wrote format 1 or 2 and no
        // layout; their files are not this layout's.
        for format in [1, 2] {
            write_raw_manifest(d.path(), &format!(r#"{{"format": {format}, "writtenBy": "0.1.1"}}"#));
            assert_eq!(
                foreign(d.path()),
                format!("vyrtel.json has no layout identity (format {format}, written by Vyrtel 0.1.1)")
            );
        }
        write_raw_manifest(d.path(), r#"{"format": 1, "layout": "0.1.3", "writtenBy": "0.1.3"}"#);
        assert_eq!(foreign(d.path()), "vyrtel.json has layout 0.1.3, written by Vyrtel 0.1.3");
        write_raw_manifest(d.path(), r#"{"layout": "0.1.0"}"#);
        assert_eq!(foreign(d.path()), "vyrtel.json has layout 0.1.0");

        // This layout, but a format it never had.
        write_raw_manifest(d.path(), &format!(r#"{{"format": 2, "layout": "{LAYOUT}", "writtenBy": "x"}}"#));
        let e = check(d.path(), true).unwrap_err();
        assert!(matches!(e, DataDirError::UnsupportedFile { found: 2, supported: 1, .. }), "{e:?}");
        assert!(e.to_string().contains("vyrtel.json uses data-directory format version 2"), "{e}");

        for bad in [
            "".to_string(),
            "{".into(),
            "[1]".into(),
            r#"{"writtenBy": "x"}"#.into(),
            r#"{"format": 1, "layout": 12, "writtenBy": "x"}"#.into(),
            format!(r#"{{"layout": "{LAYOUT}", "writtenBy": "x"}}"#),
            manifest_json(r#", "inventory": {"files": {"../outside": 1}}"#),
            manifest_json(r#", "inventory": {"files": {"metadata/metadata.db": 1}}"#),
            // Unknown keys could hide a layout this build does not know.
            manifest_json(r#", "note": "x""#),
        ] {
            write_raw_manifest(d.path(), &bad);
            assert!(matches!(check(d.path(), true), Err(DataDirError::BadManifest { .. })), "{bad:?}");
        }
    }

    #[test]
    fn metadata_of_other_layouts_is_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        record(d.path(), &Layout::Unrecorded).unwrap();
        rusqlite::Connection::open(d.path().join(METADATA_DB))
            .unwrap()
            .execute("UPDATE vyrtel_layout SET layout = '0.1.9'", [])
            .unwrap();
        assert_eq!(foreign(d.path()), "metadata/metadata.db has layout 0.1.9");

        // Without a manifest, as 0.1.1 left it.
        std::fs::remove_file(d.path().join(MANIFEST)).unwrap();
        std::fs::remove_file(d.path().join(METADATA_DB)).unwrap();
        rusqlite::Connection::open(d.path().join(METADATA_DB))
            .unwrap()
            .execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER);")
            .unwrap();
        assert!(
            foreign(d.path()).starts_with("there is no vyrtel.json, and metadata/metadata.db has no layout identity")
        );

        std::fs::write(d.path().join(METADATA_DB), b"definitely not sqlite, but long enough to look like a page")
            .unwrap();
        assert!(matches!(check(d.path(), false), Err(DataDirError::BadMetadata { .. })));
    }

    /// A WAL or segment file of another layout in a directory of this one.
    #[test]
    fn telemetry_files_of_other_layouts_are_refused() {
        let d = tempfile::tempdir().unwrap();
        complete(d.path());
        record(d.path(), &Layout::Unrecorded).unwrap();
        // A 0.1.1 WAL header: format 1 straight after the magic.
        let mut header = storage::wal::WAL_MAGIC.to_vec();
        header.extend_from_slice(&[1, 0, 2, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        std::fs::write(d.path().join("wal/traces/000000000001.wal"), header).unwrap();
        assert_eq!(
            foreign(d.path()),
            "wal/traces/000000000001.wal is a WAL file with no layout identity (bytes 0100020000000000)"
        );
    }

    #[test]
    fn the_report_says_what_starting_changes() {
        let this = env!("CARGO_PKG_VERSION");
        let can =
            format!("Vyrtel {this} (layout {LAYOUT}, data-directory format 1, metadata schema 1) can start it.\n");
        let d = tempfile::tempdir().unwrap();
        let text = report(d.path(), &check(d.path(), false).unwrap());
        assert_eq!(
            text,
            format!(
                "data directory {}\n  new: none of Vyrtel's files yet\n{can}Starting it will create a new data \
                 directory in this layout.\n",
                d.path().display()
            )
        );

        // A first start that stopped before the manifest.
        prepare(d.path()).unwrap();
        let text = report(d.path(), &check(d.path(), false).unwrap());
        assert!(text.contains("  vyrtel.json: none (a first start of this layout stopped before writing it)\n"));
        assert!(text.contains("  metadata/metadata.db: none\n"), "{text}");
        assert!(text.ends_with("Starting it will finish setting it up and record the layout in vyrtel.json.\n"));

        complete(d.path());
        std::fs::write(d.path().join("wal/logs/000000000003.wal"), [0u8; 40]).unwrap();
        record_stopped(d.path()).unwrap();
        let text = report(d.path(), &check(d.path(), false).unwrap());
        assert!(text.contains(&format!("  vyrtel.json: layout {LAYOUT}, format 1, last written by Vyrtel {this}\n")));
        assert!(text.contains("  clean-shutdown inventory: every WAL and segment file is present with its size\n"));
        assert!(text.contains(&format!("  metadata/metadata.db: layout {LAYOUT}, schema version 1\n")), "{text}");
        assert!(text.ends_with(&format!("{can}Starting it changes no file formats.\n")), "{text}");

        std::fs::remove_file(d.path().join("wal/logs/000000000003.wal")).unwrap();
        let text = report(d.path(), &check(d.path(), true).unwrap());
        assert!(
            text.contains("  accepted with --accept-incomplete-restore: wal/logs/000000000003.wal is missing\n"),
            "{text}"
        );
    }
}
