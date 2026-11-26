//! Admin-managed settings (SET-04, #253): the operational configuration an
//! administrator changes through `/api/v1/settings` or, with the server
//! stopped, `vyrtel settings`. See docs/configuration.md#admin-managed-settings.
//!
//! Values are kept in the metadata database's `settings` table under
//! [`metadata::CONFIG_PREFIX`]: one row per changed field (its canonical
//! JSON), one revision per group, and a bounded list of change records.
//! Nothing else in that table is reachable from here, so neither the
//! ingestion mode nor the protection latch can be written, exported or
//! imported as a setting.
//!
//! A value comes from, in order: the deployment (configuration file,
//! `VYRTEL_*` environment, flags), then these settings, then the default.
//! While a field can still be set by the deployment, a deployment value
//! wins and the field cannot be saved here; the settings API reports it
//! as managed by its source. Saved values are read before anything that
//! uses them is built, and every field currently applies at the next
//! restart: the running process keeps the values it started with, and a
//! saved change is reported as pending until then.
//!
//! A write validates the complete configuration it would produce, then
//! commits its values, the group revision and its change record in one
//! SQLite transaction that also checks the revision it was based on, and
//! only then publishes the result in memory. A failed or conflicting write
//! changes nothing. Requests never read SQLite for settings.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use metadata::{Metadata, MetadataError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{Config, Source};

/// When a saved value takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Apply {
    /// At the next start; the running process keeps its value until then.
    Restart,
}

/// What a field holds, for the schema and for forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A duration: `"30s"`, `"5m"`, or a number of seconds.
    Duration,
    /// A size in bytes: a number, or `"64MB"`.
    Bytes,
    /// A whole number.
    Count,
    Boolean,
    /// One of a fixed list of strings.
    Choice(&'static [&'static str]),
    /// A duration, or `"forever"`.
    Retention,
    /// A log filter such as `info` or `storage=debug,info`.
    LogFilter,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Duration => "duration",
            Kind::Bytes => "bytes",
            Kind::Count => "count",
            Kind::Boolean => "boolean",
            Kind::Choice(_) => "choice",
            Kind::Retention => "retention",
            Kind::LogFilter => "logFilter",
        }
    }
}

/// One admin-managed setting.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// `section.key`, as in the configuration file.
    pub key: &'static str,
    pub kind: Kind,
    /// Inclusive bounds as shown to people; the complete rules (including
    /// those across fields) are [`Config::validate`]'s.
    pub min: Option<&'static str>,
    pub max: Option<&'static str>,
    /// A change can lose data or weaken durability: a write that changes it
    /// must say `confirm`.
    pub dangerous: bool,
    pub apply: Apply,
    pub description: &'static str,
}

impl Field {
    pub fn group(&self) -> &'static str {
        self.key.split_once('.').map(|(g, _)| g).unwrap_or(self.key)
    }

    pub fn name(&self) -> &'static str {
        self.key.split_once('.').map(|(_, n)| n).unwrap_or(self.key)
    }
}

const fn f(
    key: &'static str,
    kind: Kind,
    min: Option<&'static str>,
    max: Option<&'static str>,
    d: &'static str,
) -> Field {
    Field { key, kind, min, max, dangerous: false, apply: Apply::Restart, description: d }
}

const fn danger(field: Field) -> Field {
    Field { dangerous: true, ..field }
}

const DURABILITY: &[&str] = &["normal", "strict"];
const LOG_FORMATS: &[&str] = &["text", "json"];

/// Every admin-managed setting, in display order. Configuration-file
/// fields that are not here are [`DEPLOYMENT`] inputs.
pub const FIELDS: &[Field] = {
    use Kind::*;
    &[
        f(
            "server.shutdown_timeout",
            Duration,
            Some("1ms"),
            None,
            "How long accepted work may take to drain after a stop request.",
        ),
        f(
            "server.max_connections",
            Count,
            Some("1"),
            Some("65536"),
            "Connections admitted at once, across every listener.",
        ),
        f(
            "server.header_timeout",
            Duration,
            Some("1ms"),
            Some("1h"),
            "How long a connection may take to send a request head; also ends idle keep-alive connections.",
        ),
        f(
            "server.body_timeout",
            Duration,
            Some("1ms"),
            Some("1h"),
            "How long a request body may take to arrive in full.",
        ),
        f(
            "server.write_timeout",
            Duration,
            Some("1ms"),
            Some("1h"),
            "How long one write may stay blocked by a client that is not reading.",
        ),
        f(
            "server.connection_buffer",
            Bytes,
            Some("8 KB"),
            Some("408 KB"),
            "Cap on each connection's read and write buffers.",
        ),
        f(
            "storage.max_memory",
            Bytes,
            Some("64 MB"),
            None,
            "Global memory budget, split between storage, ingest, queries and background work.",
        ),
        f(
            "storage.segment_target_size",
            Bytes,
            Some("64 KB"),
            None,
            "Uncompressed size at which the active segment is sealed.",
        ),
        f(
            "storage.segment_max_age",
            Duration,
            None,
            None,
            "Age at which the active segment is sealed whatever its size.",
        ),
        danger(f(
            "storage.durability",
            Choice(DURABILITY),
            None,
            None,
            "normal: fsync the WAL every fsync_interval; strict: fsync before every acknowledgement.",
        )),
        danger(f(
            "storage.fsync_interval",
            Duration,
            None,
            None,
            "How often the WAL is synced in normal durability; acknowledged writes since the last sync can be lost in a crash.",
        )),
        danger(f("storage.compaction", Boolean, None, None, "Merge small sealed segments in the background.")),
        danger(f(
            "retention.logs",
            Retention,
            None,
            None,
            "How long logs are kept; shortening it deletes older data at the next maintenance pass.",
        )),
        danger(f(
            "retention.traces",
            Retention,
            None,
            None,
            "How long traces are kept; shortening it deletes older data at the next maintenance pass.",
        )),
        danger(f(
            "retention.metrics",
            Retention,
            None,
            None,
            "How long metrics are kept; shortening it deletes older data at the next maintenance pass.",
        )),
        f("ingest.max_request_size", Bytes, Some("1 KB"), None, "Largest ingest request body, after decompression."),
        f(
            "ingest.queue_capacity",
            Count,
            Some("1"),
            None,
            "Events waiting to be written, per signal; beyond this ingest gets 429.",
        ),
        f(
            "ingest.ack_timeout",
            Duration,
            None,
            None,
            "How long an ingest request waits for its write to be acknowledged.",
        ),
        f(
            "ingest.max_concurrent",
            Count,
            Some("0"),
            None,
            "Ingest requests admitted at once; 0 derives it from the memory budget.",
        ),
        f("query.timeout", Duration, Some("1ms"), None, "Execution time of one query request (504 beyond)."),
        f("query.max_concurrent", Count, Some("1"), None, "Queries executing at once."),
        f(
            "query.queue_timeout",
            Duration,
            None,
            None,
            "How long a query waits for a free slot before 503; 0s rejects at once when busy.",
        ),
        f("query.max_scan_bytes", Bytes, None, None, "Data one query may read."),
        f("query.max_results", Count, Some("1"), None, "Results one query may return."),
        f("query.max_trace_spans", Count, Some("1"), None, "Spans one trace request may return (422 beyond)."),
        f("query.max_metric_names", Count, Some("1"), None, "Metric names one listing returns."),
        f("query.max_live_streams", Count, None, None, "Live-tail streams open at once."),
        f("auth.session_ttl", Duration, None, None, "How long a sign-in session lasts."),
        f("alerts.enabled", Boolean, None, None, "Run the alert scheduler."),
        f("alerts.webhook_timeout", Duration, None, None, "How long an alert webhook may take."),
        f("log.level", LogFilter, None, None, "Log filter, e.g. info, debug or storage=debug,info."),
        f("log.format", Choice(LOG_FORMATS), None, None, "Log output format."),
    ]
};

/// Configuration-file fields that stay deployment and bootstrap inputs:
/// the listener, the data directory (which holds this store), and the
/// first-start administrator. Never admin-managed settings, never exported.
pub const DEPLOYMENT: &[&str] =
    &["server.bind", "server.listen_backlog", "storage.path", "auth.admin_username", "auth.admin_password"];

/// Admin-managed settings that are not in [`FIELDS`] yet, with what owns
/// them meanwhile. With [`FIELDS`] they are the 33 operational settings of
/// #253's inventory; `ingest.auth_mode` keeps its own live endpoint until
/// #254 brings it under this contract. It is not a configuration-file field
/// (it never was one), so the 37 file fields are [`FIELDS`] and
/// [`DEPLOYMENT`]. `server.listen_backlog`, added to the file after that
/// inventory was taken, is a deployment input like `server.bind`.
pub const SEPARATE: &[(&str, &str)] =
    &[("ingest.auth_mode", "PUT /api/v1/ingest-auth (metadata key ingest_auth_mode), live")];

/// The groups, in display order. Each has its own revision.
pub const GROUPS: &[&str] = &["server", "storage", "retention", "ingest", "query", "auth", "alerts", "log"];

/// Change records kept (oldest dropped first), committed or refused.
pub const MAX_CHANGES: usize = 50;

/// Largest settings export accepted for import.
pub const MAX_IMPORT_BYTES: usize = 1 << 20;

const CHANGES_KEY: &str = "config.changes";
const REVISION_PREFIX: &str = "config.revision.";

/// The format name of an export.
pub const EXPORT_FORMAT: &str = "vyrtel.settings";
pub const EXPORT_VERSION: u64 = 1;

pub fn field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

fn value_key(key: &str) -> String {
    format!("{}{key}", metadata::CONFIG_PREFIX)
}

fn revision_key(group: &str) -> String {
    format!("{REVISION_PREFIX}{group}")
}

// ---- reading and writing one field of a Config ------------------------

fn to_json<T: Serialize>(section: &T) -> Value {
    serde_json::to_value(section).unwrap_or(Value::Null)
}

/// A section as JSON, its non-setting fields blanked so a path that is not
/// UTF-8 or a password never take part.
fn section_json(config: &Config, group: &str) -> Value {
    match group {
        "server" => to_json(&config.server),
        "storage" => {
            let mut s = config.storage.clone();
            s.path = PathBuf::new();
            to_json(&s)
        }
        "retention" => to_json(&config.retention),
        "ingest" => to_json(&config.ingest),
        "query" => to_json(&config.query),
        "auth" => to_json(&config.auth),
        "alerts" => to_json(&config.alerts),
        "log" => to_json(&config.log),
        _ => Value::Null,
    }
}

/// The canonical value of setting `key` in `config` (as it is stored and
/// returned): durations and retention as strings (`"30s"`, `"forever"`),
/// sizes as bytes, counts as numbers.
pub fn get(config: &Config, key: &str) -> Value {
    let Some(f) = field(key) else { return Value::Null };
    section_json(config, f.group()).get(f.name()).cloned().unwrap_or(Value::Null)
}

fn patch<T: Serialize + DeserializeOwned>(section: &mut T, name: &str, value: &Value) -> Result<(), String> {
    let mut j = to_json(&*section);
    let Some(obj) = j.as_object_mut() else { return Err("cannot be set".into()) };
    obj.insert(name.to_string(), value.clone());
    *section = serde_json::from_value(j).map_err(|e| e.to_string())?;
    Ok(())
}

/// Set setting `key` in `config` from `value`, accepted in any form the
/// configuration file accepts. Checks the value's type and syntax only;
/// [`validate`] checks the whole configuration.
pub fn set(config: &mut Config, key: &str, value: &Value) -> Result<(), String> {
    let Some(f) = field(key) else { return Err(format!("{key} is not an admin-managed setting")) };
    let name = f.name();
    let r = match f.group() {
        "server" => patch(&mut config.server, name, value),
        "storage" => {
            let path = std::mem::take(&mut config.storage.path);
            let r = patch(&mut config.storage, name, value);
            config.storage.path = path;
            r
        }
        "retention" => patch(&mut config.retention, name, value),
        "ingest" => patch(&mut config.ingest, name, value),
        "query" => patch(&mut config.query, name, value),
        "auth" => {
            let password = config.auth.admin_password.take();
            let r = patch(&mut config.auth, name, value);
            config.auth.admin_password = password;
            r
        }
        "alerts" => patch(&mut config.alerts, name, value),
        "log" => patch(&mut config.log, name, value),
        _ => Err("cannot be set".into()),
    };
    r.map_err(|e| format!("{key}: {e}"))
}

/// The complete rules for a configuration that settings produced:
/// [`Config::validate`], and a log filter that parses (the configuration
/// file falls back to `info` instead, for compatibility).
pub fn validate(config: &Config) -> Result<(), String> {
    config.validate()?;
    tracing_subscriber::EnvFilter::try_new(&config.log.level)
        .map_err(|e| format!("log.level {:?} is not a valid log filter: {e}", config.log.level))?;
    Ok(())
}

/// `config` with every setting that the deployment did not set back at its
/// default: what saved settings apply to.
///
/// A value with no recorded source that is not the default was set by the
/// code that built `config` ([`Source::Caller`]) and is kept like a
/// deployment value; one that came from [`Source::Settings`] goes back to
/// its default, so loading settings again starts from the same base.
pub fn deployment_base(config: &Config) -> Config {
    let defaults = Config::default();
    let mut base = config.clone();
    for f in FIELDS {
        let source = config.origin.get(f.key);
        if source == Source::Default && get(config, f.key) != get(&defaults, f.key) {
            base.origin.set(f.key, Source::Caller);
        } else if !source.is_deployment() {
            // A default always fits its own field.
            let _ = set(&mut base, f.key, &get(&defaults, f.key));
            base.origin.set(f.key, Source::Default);
        }
    }
    base
}

// ---- the stored state -------------------------------------------------

/// What one change did, for diagnostics. Never holds values: field names,
/// who, when and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// Milliseconds since the epoch.
    pub at: i64,
    /// `user:<name>`, `api-key:<id>` or `cli`.
    pub actor: String,
    /// What was asked: `save`, `cancel` or `import`.
    pub action: String,
    pub groups: Vec<String>,
    /// The revisions the change produced (committed), or was based on.
    pub revisions: BTreeMap<String, u64>,
    pub fields: Vec<String>,
    /// `committed`, or why it was refused (`conflict`, `invalid`,
    /// `deployment`, `confirmation_required`, `failed`).
    pub outcome: String,
    /// When the committed values take effect.
    pub apply: Apply,
}

/// The saved settings: changed fields' canonical values, group revisions
/// (0 when never written) and committed change records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stored {
    pub values: BTreeMap<String, Value>,
    pub revisions: BTreeMap<String, u64>,
    pub changes: Vec<Change>,
}

impl Stored {
    pub fn revision(&self, group: &str) -> u64 {
        self.revisions.get(group).copied().unwrap_or(0)
    }

    /// Read the rows [`Metadata::config_settings`] returns. A value this
    /// build cannot use (an unknown key, a wrong type, a revision that is
    /// not a number) is an error that says how to recover, never a default.
    pub fn parse(rows: &[(String, String)]) -> Result<Stored, String> {
        let mut stored = Stored::default();
        let mut scratch = Config::default();
        let fix = |key: &str| {
            format!(
                "with the server stopped, run `vyrtel settings reset {key}` (or restore the metadata database from a \
                 backup); see docs/configuration.md#admin-managed-settings"
            )
        };
        for (row, text) in rows {
            if row == CHANGES_KEY {
                match serde_json::from_str::<Vec<Change>>(text) {
                    Ok(changes) => stored.changes = changes,
                    // Diagnostics only: a damaged list is reported and replaced.
                    Err(e) => tracing::warn!("ignoring unreadable settings change records: {e}"),
                }
                continue;
            }
            if let Some(group) = row.strip_prefix(REVISION_PREFIX) {
                if !GROUPS.contains(&group) {
                    return Err(format!("saved settings have a revision for unknown group {group:?}"));
                }
                let n = text.parse::<u64>().map_err(|_| {
                    format!("saved settings revision of {group} is {text:?}, not a number; {}", fix(group))
                })?;
                stored.revisions.insert(group.to_string(), n);
                continue;
            }
            let key = row.strip_prefix(metadata::CONFIG_PREFIX).unwrap_or(row);
            if field(key).is_none() {
                return Err(format!("saved settings contain {key:?}, which this Vyrtel does not know; {}", fix(key)));
            }
            let value: Value = serde_json::from_str(text)
                .map_err(|_| format!("saved setting {key} ({text:?}) is unreadable; {}", fix(key)))?;
            set(&mut scratch, key, &value).map_err(|e| format!("saved setting {e}; {}", fix(key)))?;
            stored.values.insert(key.to_string(), get(&scratch, key));
        }
        Ok(stored)
    }

    /// `base` with these values applied to every field the deployment did
    /// not set, and the complete result validated.
    pub fn apply(&self, base: &Config) -> Result<Config, String> {
        let mut config = base.clone();
        for (key, value) in &self.values {
            if base.origin.get(key).is_deployment() {
                continue;
            }
            set(&mut config, key, value)?;
            config.origin.set(key, Source::Settings);
        }
        validate(&config)?;
        Ok(config)
    }

    /// Saved values the deployment overrides, with what overrides them.
    pub fn overridden(&self, base: &Config) -> Vec<(String, Source)> {
        self.values
            .iter()
            .filter(|(key, value)| base.origin.get(key).is_deployment() && get(base, key) != **value)
            .map(|(key, _)| (key.clone(), base.origin.get(key)))
            .collect()
    }
}

/// Saved settings applied to `config` (as loaded from the deployment), for
/// a start. Fails, with how to recover, if they cannot be read or the
/// result is invalid.
pub fn load(rows: &[(String, String)], config: &Config) -> Result<(Config, Stored), String> {
    let stored = Stored::parse(rows)?;
    let base = deployment_base(config);
    let effective = stored.apply(&base).map_err(|e| {
        format!(
            "saved settings do not form a valid configuration: {e}. With the server stopped, inspect them with \
             `vyrtel settings show` and fix them with `vyrtel settings set` or `vyrtel settings reset` (see \
             docs/configuration.md#admin-managed-settings)"
        )
    })?;
    Ok((effective, stored))
}

/// The configuration a start would use: `config` with the settings saved in
/// its data directory, read without changing or locking anything. For
/// logging and the other things set up before the data directory is
/// opened; the start reads them again under the lock.
pub fn for_start(config: &Config) -> Result<Config, String> {
    let rows = read_rows_at(&config.storage.path)?;
    load(&rows, config).map(|(c, _)| c)
}

fn read_rows_at(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let path = dir.join(crate::datadir::METADATA_DB);
    Metadata::read_config_settings_at(&path)
        .map(Option::unwrap_or_default)
        .map_err(|e| format!("reading saved settings from {}: {e}", path.display()))
}

// ---- changes ----------------------------------------------------------

/// A requested change.
#[derive(Debug, Clone)]
pub enum Edit {
    /// Set (`Some`) or reset to default (`None`) fields of one group, by
    /// field name, based on the group's `revision`.
    Save { group: String, revision: u64, values: BTreeMap<String, Option<Value>>, confirm: bool },
    /// Make one group's next-start values equal its running ones.
    Cancel { group: String, revision: u64 },
    /// Replace every saved value with `values` (by `section.key`), based on
    /// every group's revision in `revisions` (`None`: unchecked, offline).
    Import { revisions: Option<BTreeMap<String, u64>>, values: BTreeMap<String, Value>, confirm: bool },
    /// Return `keys` (`section.key`) to their defaults, unchecked: offline
    /// recovery, which may also drop saved values that no longer parse.
    Reset { keys: Vec<String>, confirm: bool },
}

impl Edit {
    fn action(&self) -> &'static str {
        match self {
            Edit::Save { .. } => "save",
            Edit::Cancel { .. } => "cancel",
            Edit::Import { .. } => "import",
            Edit::Reset { .. } => "reset",
        }
    }
}

/// Why a change was refused. Nothing was written.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0} is not a settings group")]
    UnknownGroup(String),
    #[error("{0}")]
    Unknown(String),
    #[error("{0}")]
    Invalid(String),
    #[error(
        "{key} is set by the {by}, which takes precedence over saved settings; remove it there to manage it in \
         Settings"
    )]
    Deployment { key: String, by: &'static str },
    #[error("the {group} settings changed since revision {expected} (now {current}); reload them and try again")]
    Stale { group: String, expected: u64, current: u64 },
    #[error("changing {} can lose data or weaken durability; confirm the change to save it", .0.join(", "))]
    ConfirmationRequired(Vec<String>),
    #[error("saving settings failed (nothing was saved): {0}")]
    Store(MetadataError),
}

impl SettingsError {
    fn outcome(&self) -> &'static str {
        match self {
            SettingsError::Stale { .. } => "conflict",
            SettingsError::Deployment { .. } => "deployment",
            SettingsError::ConfirmationRequired(_) => "confirmation_required",
            SettingsError::Store(_) => "failed",
            _ => "invalid",
        }
    }
}

fn source_name(s: Source) -> &'static str {
    match s {
        Source::File => "configuration file",
        Source::Environment => "environment",
        Source::Flag => "command line",
        Source::Caller => "program that started Vyrtel",
        Source::Default | Source::Settings => "settings",
    }
}

/// A checked change, ready to commit.
#[derive(Debug)]
pub struct Plan {
    pub stored: Stored,
    pub groups: Vec<String>,
    pub fields: Vec<String>,
    /// The configuration the next start will use.
    pub next: Config,
    checks: Vec<(String, Option<String>)>,
    writes: Vec<(String, Option<String>)>,
    based_on: BTreeMap<String, u64>,
}

fn check_group(group: &str) -> Result<(), SettingsError> {
    if GROUPS.contains(&group) { Ok(()) } else { Err(SettingsError::UnknownGroup(group.to_string())) }
}

fn check_revision(stored: &Stored, group: &str, expected: u64) -> Result<(), SettingsError> {
    let current = stored.revision(group);
    if current != expected {
        return Err(SettingsError::Stale { group: group.to_string(), expected, current });
    }
    Ok(())
}

/// Check `edit` against the saved settings (`stored`), the deployment
/// (`base`) and the running configuration, and work out what to write.
pub fn plan(base: &Config, running: &Config, stored: &Stored, edit: &Edit) -> Result<Plan, SettingsError> {
    let mut values = stored.values.clone();
    let mut based_on = BTreeMap::new();
    let mut confirm = false;
    match edit {
        Edit::Save { group, revision, values: changes, confirm: c } => {
            check_group(group)?;
            check_revision(stored, group, *revision)?;
            based_on.insert(group.clone(), *revision);
            confirm = *c;
            if changes.is_empty() {
                return Err(SettingsError::Invalid("no settings to save".into()));
            }
            let mut scratch = base.clone();
            for (name, value) in changes {
                let key = format!("{group}.{name}");
                if field(&key).is_none() {
                    return Err(SettingsError::Unknown(not_a_setting(&key)));
                }
                let source = base.origin.get(&key);
                if source.is_deployment() {
                    return Err(SettingsError::Deployment { key, by: source_name(source) });
                }
                match value {
                    Some(v) => {
                        set(&mut scratch, &key, v).map_err(SettingsError::Invalid)?;
                        values.insert(key.clone(), get(&scratch, &key));
                    }
                    None => {
                        values.remove(&key);
                    }
                }
            }
        }
        Edit::Cancel { group, revision } => {
            check_group(group)?;
            check_revision(stored, group, *revision)?;
            based_on.insert(group.clone(), *revision);
            let defaults = Config::default();
            for f in FIELDS.iter().filter(|f| f.group() == group && !base.origin.get(f.key).is_deployment()) {
                let now = get(running, f.key);
                if now == get(&defaults, f.key) {
                    values.remove(f.key);
                } else {
                    values.insert(f.key.to_string(), now);
                }
            }
        }
        Edit::Import { revisions, values: imported, confirm: c } => {
            if let Some(revisions) = revisions {
                for group in GROUPS {
                    let expected = revisions.get(*group).copied().unwrap_or(0);
                    check_revision(stored, group, expected)?;
                    based_on.insert(group.to_string(), expected);
                }
            }
            confirm = *c;
            let mut scratch = base.clone();
            values.clear();
            for (key, value) in imported {
                if field(key).is_none() {
                    return Err(SettingsError::Unknown(not_a_setting(key)));
                }
                let source = base.origin.get(key);
                if source.is_deployment() {
                    return Err(SettingsError::Deployment { key: key.clone(), by: source_name(source) });
                }
                set(&mut scratch, key, value).map_err(SettingsError::Invalid)?;
                values.insert(key.clone(), get(&scratch, key));
            }
        }
        Edit::Reset { keys, confirm: c } => {
            confirm = *c;
            for key in keys {
                if field(key).is_none() {
                    return Err(SettingsError::Unknown(not_a_setting(key)));
                }
                values.remove(key);
            }
        }
    }
    let next_stored = Stored { values, revisions: stored.revisions.clone(), changes: stored.changes.clone() };
    let next = next_stored.apply(base).map_err(SettingsError::Invalid)?;
    let now = stored.apply(base).unwrap_or_else(|_| running.clone());

    let changed: Vec<&Field> = FIELDS
        .iter()
        .filter(|f| stored.values.get(f.key) != next_stored.values.get(f.key) || get(&now, f.key) != get(&next, f.key))
        .collect();
    let dangerous: Vec<String> = changed
        .iter()
        .filter(|f| f.dangerous && get(&now, f.key) != get(&next, f.key))
        .map(|f| f.key.to_string())
        .collect();
    if !dangerous.is_empty() && !confirm && !matches!(edit, Edit::Cancel { .. }) {
        return Err(SettingsError::ConfirmationRequired(dangerous));
    }

    let mut groups: BTreeSet<String> = changed.iter().map(|f| f.group().to_string()).collect();
    if let Edit::Save { group, .. } | Edit::Cancel { group, .. } = edit {
        groups.insert(group.clone());
    }
    let groups: Vec<String> = GROUPS.iter().filter(|g| groups.contains(**g)).map(|g| g.to_string()).collect();
    let mut checks = Vec::new();
    let mut writes = Vec::new();
    let mut revisions = stored.revisions.clone();
    for g in &groups {
        let current = stored.revision(g);
        checks.push((revision_key(g), (current > 0).then(|| current.to_string())));
        writes.push((revision_key(g), Some((current + 1).to_string())));
        revisions.insert(g.clone(), current + 1);
    }
    for f in &changed {
        if stored.values.get(f.key) != next_stored.values.get(f.key) {
            writes.push((value_key(f.key), next_stored.values.get(f.key).map(Value::to_string)));
        }
    }
    Ok(Plan {
        stored: Stored { revisions, ..next_stored },
        groups,
        fields: changed.iter().map(|f| f.key.to_string()).collect(),
        next,
        checks,
        writes,
        based_on,
    })
}

fn not_a_setting(key: &str) -> String {
    if DEPLOYMENT.contains(&key) {
        format!("{key} is a deployment input, set in the configuration file or environment; it is not a setting")
    } else {
        format!("{key} is not an admin-managed setting")
    }
}

fn push_change(changes: &mut Vec<Change>, change: Change) {
    changes.push(change);
    if changes.len() > MAX_CHANGES {
        let extra = changes.len() - MAX_CHANGES;
        changes.drain(..extra);
    }
}

/// Commit `plan` to `m` with its change record: one transaction that also
/// checks the revisions it was based on. Returns the new saved state.
pub fn commit(m: &Metadata, mut plan: Plan, actor: &str, action: &str) -> Result<Stored, SettingsError> {
    let change = Change {
        at: metadata::now_ms(),
        actor: actor.to_string(),
        action: action.to_string(),
        groups: plan.groups.clone(),
        revisions: plan.groups.iter().map(|g| (g.clone(), plan.stored.revision(g))).collect(),
        fields: plan.fields.clone(),
        outcome: "committed".into(),
        apply: Apply::Restart,
    };
    push_change(&mut plan.stored.changes, change);
    let changes = serde_json::to_string(&plan.stored.changes).map_err(|e| SettingsError::Invalid(e.to_string()))?;
    plan.writes.push((CHANGES_KEY.to_string(), Some(changes)));
    match m.commit_config_settings(&plan.checks, &plan.writes) {
        Ok(()) => Ok(plan.stored),
        // Only another writer (offline, or a race) gets here: the revision moved.
        Err(MetadataError::Conflict(_)) => {
            let group = plan.groups.first().cloned().unwrap_or_default();
            let expected = plan.based_on.get(&group).copied().unwrap_or(0);
            Err(SettingsError::Stale { group, expected, current: expected + 1 })
        }
        Err(e) => Err(SettingsError::Store(e)),
    }
}

// ---- the running server's view ----------------------------------------

/// Saved settings as the running server sees them: kept in memory, changed
/// only by [`Settings::change`], which commits before it publishes.
pub struct Settings {
    /// The deployment's values over the defaults.
    base: Config,
    stored: RwLock<Stored>,
    /// Refused changes since the start (memory only, bounded).
    refused: Mutex<VecDeque<Change>>,
    /// One change at a time, so publication follows commit order.
    writer: tokio::sync::Mutex<()>,
}

impl Settings {
    pub fn new(config: &Config, stored: Stored) -> Self {
        Self {
            base: deployment_base(config),
            stored: RwLock::new(stored),
            refused: Mutex::new(VecDeque::new()),
            writer: tokio::sync::Mutex::new(()),
        }
    }

    pub fn stored(&self) -> Stored {
        self.stored.read().unwrap().clone()
    }

    /// Validate, commit and publish `edit` by `actor`. The metadata write
    /// runs on the blocking pool; no request lock is held meanwhile.
    pub async fn change(
        &self,
        m: &std::sync::Arc<Metadata>,
        running: &Config,
        actor: String,
        edit: Edit,
    ) -> Result<Stored, SettingsError> {
        let _one = self.writer.lock().await;
        let stored = self.stored();
        let action = edit.action();
        let result = match plan(&self.base, running, &stored, &edit) {
            Ok(plan) => {
                let m = m.clone();
                let who = actor.clone();
                tokio::task::spawn_blocking(move || commit(&m, plan, &who, action))
                    .await
                    .unwrap_or_else(|e| Err(SettingsError::Invalid(format!("saving settings failed: {e}"))))
            }
            Err(e) => Err(e),
        };
        match &result {
            Ok(next) => {
                *self.stored.write().unwrap() = next.clone();
                let last = next.changes.last();
                tracing::info!(
                    actor = %actor,
                    action,
                    groups = ?last.map(|c| &c.groups),
                    fields = ?last.map(|c| &c.fields),
                    "settings saved; they apply at the next restart"
                );
            }
            Err(e) => {
                let (groups, fields) = match &edit {
                    Edit::Save { group, values, .. } => {
                        (vec![group.clone()], values.keys().map(|n| format!("{group}.{n}")).collect())
                    }
                    Edit::Cancel { group, .. } => (vec![group.clone()], Vec::new()),
                    Edit::Import { values, .. } => (Vec::new(), values.keys().cloned().collect()),
                    Edit::Reset { keys, .. } => (Vec::new(), keys.clone()),
                };
                // Names only, bounded: a refused request may name anything.
                let fields: Vec<String> = fields.into_iter().take(FIELDS.len()).map(|f| truncate(&f, 80)).collect();
                let groups: Vec<String> = groups.into_iter().map(|g| truncate(&g, 40)).collect();
                tracing::info!(actor = %actor, action, outcome = e.outcome(), ?fields, "settings change refused");
                let mut refused = self.refused.lock().unwrap();
                refused.push_back(Change {
                    at: metadata::now_ms(),
                    actor,
                    action: action.into(),
                    revisions: groups.iter().map(|g| (g.clone(), stored.revision(g))).collect(),
                    groups,
                    fields,
                    outcome: e.outcome().into(),
                    apply: Apply::Restart,
                });
                while refused.len() > MAX_CHANGES {
                    refused.pop_front();
                }
            }
        }
        result
    }

    /// The `GET /api/v1/settings` document for a server running `running`.
    pub fn document(&self, running: &Config) -> Value {
        let stored = self.stored();
        let mut changes: Vec<Change> = stored.changes.clone();
        changes.extend(self.refused.lock().unwrap().iter().cloned());
        changes.sort_by_key(|c| c.at);
        let skip = changes.len().saturating_sub(MAX_CHANGES);
        document(&self.base, running, &stored, changes.into_iter().skip(skip).collect())
    }

    pub fn export(&self) -> Value {
        export(&self.stored())
    }
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The settings document: every group with its revision and fields, the
/// deployment inputs (sources only for the password) and change records.
pub fn document(base: &Config, running: &Config, stored: &Stored, changes: Vec<Change>) -> Value {
    let defaults = Config::default();
    let next = stored.apply(base).ok();
    let mut restart_required = false;
    let groups: Vec<Value> = GROUPS
        .iter()
        .map(|group| {
            let fields: Vec<Value> = FIELDS
                .iter()
                .filter(|f| f.group() == *group)
                .map(|f| {
                    let effective = get(running, f.key);
                    let deployment = base.origin.get(f.key);
                    let next_value = next.as_ref().map(|n| get(n, f.key));
                    let pending = next_value.as_ref().filter(|v| **v != effective).cloned();
                    restart_required |= pending.is_some();
                    let choices = match f.kind {
                        Kind::Choice(c) => json!(c),
                        _ => Value::Null,
                    };
                    json!({
                        "key": f.key,
                        "name": f.name(),
                        "type": f.kind.name(),
                        "choices": choices,
                        "min": f.min,
                        "max": f.max,
                        "description": f.description,
                        "dangerous": f.dangerous,
                        "apply": f.apply,
                        "default": get(&defaults, f.key),
                        "configured": stored.values.get(f.key),
                        "effective": effective,
                        "source": running.origin.get(f.key),
                        "pending": pending,
                        "managedBy": deployment.is_deployment().then_some(deployment),
                        "overridden": deployment.is_deployment() && stored.values.contains_key(f.key),
                    })
                })
                .collect();
            json!({ "name": group, "revision": stored.revision(group), "fields": fields })
        })
        .collect();
    let deployment: Vec<Value> = DEPLOYMENT
        .iter()
        .map(|key| {
            let source = base.origin.get(key);
            let value = match *key {
                "server.bind" => json!(running.server.bind),
                "server.listen_backlog" => json!(running.server.listen_backlog),
                "storage.path" => json!(running.storage.path.display().to_string()),
                "auth.admin_username" => json!(running.auth.admin_username),
                // Never the password: whether one is set, and where.
                _ => Value::Null,
            };
            let mut entry = json!({ "key": key, "source": source, "value": value });
            if *key == "auth.admin_password" {
                entry["set"] = json!(running.auth.admin_password.is_some());
                entry["firstStartOnly"] = json!(true);
            }
            entry
        })
        .collect();
    json!({
        "groups": groups,
        "restartRequired": restart_required,
        "valid": next.is_some(),
        "deployment": deployment,
        "changes": changes,
    })
}

/// A settings export: saved values only (no deployment inputs, credentials,
/// latch or revisions), by `section.key`.
pub fn export(stored: &Stored) -> Value {
    json!({
        "format": EXPORT_FORMAT,
        "version": EXPORT_VERSION,
        "vyrtel": env!("CARGO_PKG_VERSION"),
        "exportedAt": metadata::now_ms(),
        "settings": stored.values,
    })
}

/// The `settings` of an export, checked for its format.
pub fn import_values(doc: &Value) -> Result<BTreeMap<String, Value>, SettingsError> {
    let invalid = |m: &str| SettingsError::Invalid(m.to_string());
    if doc.get("format").and_then(Value::as_str) != Some(EXPORT_FORMAT) {
        return Err(invalid("not a Vyrtel settings export (its format must be \"vyrtel.settings\")"));
    }
    if doc.get("version").and_then(Value::as_u64) != Some(EXPORT_VERSION) {
        return Err(invalid("unsupported settings export version (this Vyrtel reads version 1)"));
    }
    let Some(settings) = doc.get("settings").and_then(Value::as_object) else {
        return Err(invalid("the export has no settings object"));
    };
    if settings.len() > FIELDS.len() {
        return Err(invalid("the export has more settings than this Vyrtel has"));
    }
    Ok(settings.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

// ---- offline (vyrtel settings) ----------------------------------------

/// Saved settings of the data directory, read without changing anything.
pub fn read_offline(config: &Config) -> Result<Stored, String> {
    Stored::parse(&read_rows_at(&config.storage.path)?)
}

/// Commit `edit` to a stopped server's data directory, as `cli`. Holds the
/// data directory lock throughout, so it fails while a server runs. Unlike
/// the API it can repair a store that no longer parses: [`Edit::Reset`]
/// drops its keys without parsing them. A [`Edit::Save`] revision of `None`
/// uses the current one.
pub fn change_offline(config: &Config, mut edit: Edit) -> Result<Stored, String> {
    let drop_keys = match &edit {
        Edit::Reset { keys, .. } => keys.clone(),
        _ => Vec::new(),
    };
    let dir = &config.storage.path;
    let db = dir.join(crate::datadir::METADATA_DB);
    if !db.exists() {
        return Err(format!(
            "{} has no metadata database ({}); start Vyrtel once to create it",
            dir.display(),
            crate::datadir::METADATA_DB
        ));
    }
    crate::datadir::check(dir, false).map_err(|e| e.to_string())?;
    let _lock = storage::DataDirLock::acquire(dir).map_err(|e| format!("locking the data directory: {e}"))?;
    crate::datadir::check(dir, false).map_err(|e| e.to_string())?;
    let m = Metadata::open(&db).map_err(|e| format!("opening the metadata database: {e}"))?;
    let rows = m.config_settings().map_err(|e| e.to_string())?;
    // Keys being reset are dropped before parsing, so a damaged one can go.
    let dropped: Vec<String> = drop_keys.iter().map(|k| value_key(k)).collect();
    let kept: Vec<(String, String)> = rows.iter().filter(|(k, _)| !dropped.contains(k)).cloned().collect();
    let mut stored = Stored::parse(&kept)?;
    if let Edit::Save { group, revision, .. } = &mut edit {
        // Offline the lock already excludes every other writer.
        *revision = stored.revision(group);
    }
    let base = deployment_base(config);
    let running = stored.apply(&base).unwrap_or_else(|_| base.clone());
    for (key, value) in &stored.values {
        if base.origin.get(key).is_deployment() && get(&base, key) != *value {
            eprintln!("vyrtel: note: {key} is also set by the {}, which wins", source_name(base.origin.get(key)));
        }
    }
    let mut p = plan(&base, &running, &stored, &edit).map_err(|e| e.to_string())?;
    // Deleting the damaged rows is part of the same transaction.
    for key in &dropped {
        if rows.iter().any(|(k, _)| k == key) && !p.writes.iter().any(|(k, _)| k == key) {
            p.writes.push((key.clone(), None));
            let name = key.strip_prefix(metadata::CONFIG_PREFIX).unwrap_or(key).to_string();
            if !p.fields.contains(&name) {
                p.fields.push(name);
            }
        }
    }
    stored = commit(&m, p, "cli", edit.action()).map_err(|e| e.to_string())?;
    Ok(stored)
}

/// `vyrtel settings show`: every setting with the value the next start
/// uses and where it comes from. `changed_only` lists only those not at
/// their default (`vyrtel settings diff`).
pub fn describe(config: &Config, stored: &Stored, changed_only: bool) -> String {
    let base = deployment_base(config);
    let defaults = Config::default();
    let mut out = String::new();
    let next = match stored.apply(&base) {
        Ok(c) => c,
        Err(e) => {
            out.push_str(&format!("saved settings are not valid: {e}\n"));
            base.clone()
        }
    };
    for f in FIELDS {
        let value = get(&next, f.key);
        let source = if base.origin.get(f.key).is_deployment() {
            base.origin.get(f.key)
        } else if stored.values.contains_key(f.key) {
            Source::Settings
        } else {
            Source::Default
        };
        if changed_only && value == get(&defaults, f.key) && source == Source::Default {
            continue;
        }
        let mut line = format!("{:<28} {:<14} {}", f.key, display(&value), source_label(source));
        if stored.values.contains_key(f.key) && base.origin.get(f.key).is_deployment() {
            line.push_str(&format!(" (saved {} is overridden)", display(&stored.values[f.key])));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    for key in DEPLOYMENT {
        let source = base.origin.get(key);
        let value = match *key {
            "server.bind" => config.server.bind.clone(),
            "server.listen_backlog" => config.server.listen_backlog.to_string(),
            "storage.path" => config.storage.path.display().to_string(),
            "auth.admin_username" => config.auth.admin_username.clone(),
            _ => {
                if config.auth.admin_password.is_some() {
                    "(set)".into()
                } else {
                    "(not set)".into()
                }
            }
        };
        if changed_only && source == Source::Default {
            continue;
        }
        out.push_str(&format!("{key:<28} {value:<14} {} (deployment input)\n", source_label(source)));
    }
    out
}

fn source_label(s: Source) -> &'static str {
    match s {
        Source::Default => "default",
        Source::File => "file",
        Source::Environment => "environment",
        Source::Flag => "flag",
        Source::Settings => "settings",
        Source::Caller => "caller",
    }
}

fn display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A value given on the command line: a number or `true`/`false` as such,
/// anything else as a string (`30s`, `64MB`, `forever`, `debug`).
pub fn parse_cli_value(s: &str) -> Value {
    let t = s.trim();
    if let Ok(n) = t.parse::<u64>() {
        return json!(n);
    }
    match t {
        "true" => json!(true),
        "false" => json!(false),
        _ => json!(t),
    }
}

#[cfg(test)]
mod tests;
