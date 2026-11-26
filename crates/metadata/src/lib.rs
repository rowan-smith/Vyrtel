//! Product metadata in an embedded SQLite database.
//!
//! SQLite holds dashboards, alerts, API keys, sessions, settings and
//! aggregated query statistics. It never holds telemetry: the event stream
//! lives in the storage engine's WAL and segments.
//!
//! The API is synchronous; the server calls it from blocking tasks. One
//! connection behind a mutex is plenty for metadata-sized workloads.
//!
//! A database carries the layout identity ([`telemetry::LAYOUT`]): SQLite's
//! `application_id` is [`APPLICATION_ID`] and the one-row `vyrtel_layout`
//! table names the layout and [`SCHEMA_VERSION`]. A new database gets both
//! with its schema, in one transaction; any other database (another
//! layout, or one from before 0.1.2 without an identity) is refused before
//! it is written.

mod schema;

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    /// The write would break a uniqueness rule (an existing username).
    #[error("{0}")]
    Conflict(String),
    /// The database is not of this build's layout: another identity, or
    /// none. Opening it is refused before anything is written.
    #[error(
        "the metadata database has {found}, but this Vyrtel reads layout {} (schema version {SCHEMA_VERSION}) only",
        telemetry::LAYOUT
    )]
    ForeignLayout { found: String },
    /// Creating the schema of a new database failed. It is one
    /// transaction, so nothing of it was created; the next open retries.
    #[error("creating the metadata schema failed (nothing was created): {0}")]
    Create(rusqlite::Error),
}

/// The schema version of layout [`telemetry::LAYOUT`]: the only one.
pub const SCHEMA_VERSION: i64 = 1;

/// SQLite's `application_id` of a Vyrtel metadata database: "VYRT".
pub const APPLICATION_ID: i32 = 0x5659_5254;

/// What a metadata database is, judged by its identity alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// No schema yet (a new or empty file).
    Empty,
    /// Layout [`telemetry::LAYOUT`], schema [`SCHEMA_VERSION`].
    Current,
    /// Anything else, described for an error message.
    Foreign(String),
}

/// Identify the database on `conn`, reading only.
fn identify(conn: &Connection) -> Result<Identity> {
    let tables: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'", [], |r| r.get(0))?;
    let app_id: i32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if tables == 0 && app_id == 0 {
        return Ok(Identity::Empty);
    }
    let has = |name: &str| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [name],
            |r| r.get(0),
        )?)
    };
    if app_id != APPLICATION_ID || !has("vyrtel_layout")? {
        return Ok(Identity::Foreign(if has("schema_migrations")? {
            "no layout identity (it has schema_migrations: Vyrtel 0.1.1 or earlier, or Observer, wrote it)".into()
        } else {
            "no layout identity (it is not a Vyrtel metadata database)".into()
        }));
    }
    let row: Option<(String, i64)> = conn
        .query_row("SELECT layout, schema_version FROM vyrtel_layout WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(match row {
        None => Identity::Foreign("no layout identity (vyrtel_layout is empty)".into()),
        Some((layout, _)) if layout != telemetry::LAYOUT => Identity::Foreign(format!("layout {layout}")),
        Some((_, v)) if v != SCHEMA_VERSION => {
            Identity::Foreign(format!("layout {} schema version {v}", telemetry::LAYOUT))
        }
        Some(_) => Identity::Current,
    })
}

pub type Result<T> = std::result::Result<T, MetadataError>;

/// The `settings` key of the ingestion authentication mode.
pub const INGEST_AUTH_MODE: &str = "ingest_auth_mode";

/// Whether ingestion needs an API key. Set from Settings (or
/// `PUT /api/v1/ingest-auth`) and kept here only; a store without it is
/// [`IngestAuthMode::Automatic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum IngestAuthMode {
    /// Open until the first ingest-scoped key is created, then protected
    /// for good: revoking keys never opens it again.
    #[default]
    Automatic,
    /// Always protected, even before any key exists.
    Required,
    /// Always open, whatever keys exist. Keeps the protection latch, so
    /// returning to `automatic` protects ingestion again.
    Open,
}

impl IngestAuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            IngestAuthMode::Automatic => "automatic",
            IngestAuthMode::Required => "required",
            IngestAuthMode::Open => "open",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "automatic" => Some(IngestAuthMode::Automatic),
            "required" => Some(IngestAuthMode::Required),
            "open" => Some(IngestAuthMode::Open),
            _ => None,
        }
    }
}

/// The `settings` key of the ingestion protection latch.
pub const INGEST_PROTECTION: &str = "ingest_protection";

/// The only value [`INGEST_PROTECTION`] has: latched.
pub const INGEST_LATCHED: &str = "latched";

/// Latch ingestion protection in `tx`. A latch already there is kept as it
/// is, with the time it was first set.
fn latch_ingest(tx: &rusqlite::Transaction, now: i64) -> rusqlite::Result<()> {
    tx.prepare_cached("INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO NOTHING")?
        .execute(params![INGEST_PROTECTION, INGEST_LATCHED, now])?;
    Ok(())
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub struct Metadata {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub scope: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    /// When the key stops being accepted (ms since the epoch); `None` for
    /// never. Fixed at creation.
    pub expires_at: Option<i64>,
    /// The key this one was created to replace ([`Metadata::rotate_api_key`]).
    pub rotated_from: Option<i64>,
}

/// An active key as the server's registry loads it: never its plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveApiKey {
    pub id: i64,
    pub key_hash: String,
    pub scope: String,
    pub expires_at: Option<i64>,
}

/// The scopes a key can have: `ingest` sends telemetry, `query` reads it,
/// `admin` does everything (docs/api.md#operation-scopes).
pub const API_KEY_SCOPES: [&str; 3] = ["ingest", "query", "admin"];

/// Longest API key name, in characters (after trimming).
pub const MAX_API_KEY_NAME_CHARS: usize = 100;

/// Latest expiry a key may be given: the end of year 9999 (ms since the
/// epoch), so every expiry is a date the UI and clients can show.
pub const MAX_API_KEY_EXPIRY_MS: i64 = 253_402_300_799_999;

fn normalize_api_key_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        return Err(MetadataError::Invalid("key name is required".into()));
    }
    if n.chars().count() > MAX_API_KEY_NAME_CHARS {
        return Err(MetadataError::Invalid(format!("key name must be at most {MAX_API_KEY_NAME_CHARS} characters")));
    }
    if n.chars().any(char::is_control) {
        return Err(MetadataError::Invalid("key name must not contain control characters".into()));
    }
    Ok(n.to_string())
}

fn check_api_key_expiry(expires_at: Option<i64>, now: i64) -> Result<()> {
    match expires_at {
        Some(at) if at <= now => Err(MetadataError::Invalid("expiresAt must be in the future".into())),
        Some(at) if at > MAX_API_KEY_EXPIRY_MS => {
            Err(MetadataError::Invalid("expiresAt must be no later than 9999-12-31T23:59:59.999Z".into()))
        }
        _ => Ok(()),
    }
}

const API_KEY_COLUMNS: &str = "id, name, prefix, scope, created_at, last_used_at, expires_at, rotated_from";

fn api_key_from_row(r: &Row) -> rusqlite::Result<ApiKey> {
    Ok(ApiKey {
        id: r.get("id")?,
        name: r.get("name")?,
        prefix: r.get("prefix")?,
        scope: r.get("scope")?,
        created_at: r.get("created_at")?,
        last_used_at: r.get("last_used_at")?,
        expires_at: r.get("expires_at")?,
        rotated_from: r.get("rotated_from")?,
    })
}

/// A key to insert ([`insert_api_key`]).
struct NewKey<'a> {
    name: &'a str,
    prefix: &'a str,
    key_hash: &'a str,
    scope: &'a str,
    expires_at: Option<i64>,
    rotated_from: Option<i64>,
}

/// Insert a key in `tx`; an ingest-scoped one also latches ingestion.
fn insert_api_key(tx: &rusqlite::Transaction, k: NewKey, now: i64) -> Result<ApiKey> {
    tx.prepare_cached(
        "INSERT INTO api_keys (name, prefix, key_hash, scope, created_at, expires_at, rotated_from) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?
    .execute(params![k.name, k.prefix, k.key_hash, k.scope, now, k.expires_at, k.rotated_from])?;
    let id = tx.last_insert_rowid();
    if k.scope == "ingest" {
        latch_ingest(tx, now)?;
    }
    Ok(ApiKey {
        id,
        name: k.name.to_string(),
        prefix: k.prefix.to_string(),
        scope: k.scope.to_string(),
        created_at: now,
        last_used_at: None,
        expires_at: k.expires_at,
        rotated_from: k.rotated_from,
    })
}

/// A local account.
///
/// `id` never changes and is never reused. `username` is case-normalized
/// ([`normalize_username`]) and unique. The password hash is never
/// serialized and `Debug` redacts it, so neither an API response nor a log
/// line built from a `User` can carry it.
#[derive(Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(skip)]
    pub password_hash: String,
}

impl std::fmt::Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("username", &self.username)
            .field("display_name", &self.display_name)
            .field("active", &self.active)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("password_hash", &"<redacted>")
            .finish()
    }
}

/// Characters a username or display name may have at most.
/// Most sessions one account keeps. Starting another ends its oldest
/// ([`Metadata::issue_session`]), so session storage is bounded by the
/// number of accounts whatever clients do.
pub const MAX_SESSIONS_PER_USER: usize = 32;

/// A session to start ([`Metadata::issue_session`]).
#[derive(Clone, Copy)]
pub struct NewSession<'a> {
    /// The SHA-256 of the session token; the token itself is never stored.
    pub token_hash: &'a str,
    pub user_id: i64,
    /// The password hash a login verified, required to still be the
    /// account's; `None` checks only that the account is active.
    pub verified_hash: Option<&'a str>,
    /// The session's lifetime from now.
    pub ttl_ms: i64,
    /// The session the client presented, which this one replaces.
    pub replaces: Option<&'a str>,
}

pub const MAX_USERNAME_CHARS: usize = 64;
pub const MAX_DISPLAY_NAME_CHARS: usize = 128;

/// The stored form of a username: trimmed and lowercased (Unicode simple
/// lowercase), so `Admin`, ` admin ` and `ADMIN` are one account. It must
/// be 1 to [`MAX_USERNAME_CHARS`] characters once lowercased (some
/// characters lowercase to two: `İ` is `i` and a combining dot), with no
/// whitespace or control characters inside.
pub fn normalize_username(username: &str) -> Result<String> {
    let u = username.trim();
    if u.is_empty() {
        return Err(MetadataError::Invalid("username is required".into()));
    }
    if u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(MetadataError::Invalid("username must not contain spaces or control characters".into()));
    }
    // Counted before lowercasing too, so a huge input is refused without
    // building its lowercase copy.
    let too_long = || MetadataError::Invalid(format!("username must be at most {MAX_USERNAME_CHARS} characters"));
    if u.chars().count() > MAX_USERNAME_CHARS {
        return Err(too_long());
    }
    let lower = u.to_lowercase();
    if lower.chars().count() > MAX_USERNAME_CHARS {
        return Err(too_long());
    }
    Ok(lower)
}

/// A display name as stored: trimmed, 1 to [`MAX_DISPLAY_NAME_CHARS`]
/// characters, no control characters. Its casing is kept.
fn normalize_display_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        return Err(MetadataError::Invalid("display name is required".into()));
    }
    if n.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(MetadataError::Invalid(format!(
            "display name must be at most {MAX_DISPLAY_NAME_CHARS} characters"
        )));
    }
    if n.chars().any(char::is_control) {
        return Err(MetadataError::Invalid("display name must not contain control characters".into()));
    }
    Ok(n.to_string())
}

const USER_COLUMNS: &str = "id, username, display_name, active, created_at, updated_at, password_hash";

fn user_from_row(r: &Row) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get("id")?,
        username: r.get("username")?,
        display_name: r.get("display_name")?,
        active: r.get("active")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        password_hash: r.get("password_hash")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    LogCount,
    MetricLine,
    SingleStat,
    SavedQuery,
}

impl PanelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PanelKind::LogCount => "log_count",
            PanelKind::MetricLine => "metric_line",
            PanelKind::SingleStat => "single_stat",
            PanelKind::SavedQuery => "saved_query",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "log_count" => Some(PanelKind::LogCount),
            "metric_line" => Some(PanelKind::MetricLine),
            "single_stat" => Some(PanelKind::SingleStat),
            "saved_query" => Some(PanelKind::SavedQuery),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    pub id: i64,
    pub dashboard_id: i64,
    pub title: String,
    pub kind: PanelKind,
    /// Kind-specific settings (query, metric name, aggregation...).
    pub config: serde_json::Value,
    pub position: i64,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelInput {
    pub title: String,
    pub kind: PanelKind,
    #[serde(default)]
    pub config: serde_json::Value,
    pub position: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub panels: Vec<Panel>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    pub id: i64,
    pub name: String,
    pub signal: String,
    pub query: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AlertStatus {
    Ok,
    Firing,
    Error,
}

impl AlertStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AlertStatus::Ok => "OK",
            AlertStatus::Firing => "FIRING",
            AlertStatus::Error => "ERROR",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "OK" => Some(AlertStatus::Ok),
            "FIRING" => Some(AlertStatus::Firing),
            "ERROR" => Some(AlertStatus::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AlertInput {
    pub name: String,
    pub query: String,
    /// One of `>`, `>=`, `<`, `<=`, `=`, `!=`.
    pub op: String,
    pub threshold: f64,
    pub window_secs: i64,
    pub interval_secs: i64,
    pub webhook_url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AlertState {
    pub status: AlertStatus,
    pub last_value: Option<f64>,
    pub last_evaluated_at: Option<i64>,
    pub last_transition_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_notification_at: Option<i64>,
    pub last_notification_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: i64,
    #[serde(flatten)]
    pub def: AlertInput,
    pub created_at: i64,
    pub updated_at: i64,
    pub state: AlertState,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AlertTransition {
    pub from: String,
    pub to: String,
    pub value: Option<f64>,
    pub message: Option<String>,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct StoredQueryStats {
    pub signal: String,
    pub field: String,
    pub index: String,
    pub queries: u64,
    pub bytes_read: u64,
    pub segments_scanned: u64,
    pub segments_skipped: u64,
    pub total_ms: f64,
    pub last_seen: i64,
}

pub const ALERT_OPS: [&str; 6] = [">", ">=", "<", "<=", "=", "!="];

impl AlertInput {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(MetadataError::Invalid("alert name is required".into()));
        }
        if !ALERT_OPS.contains(&self.op.as_str()) {
            return Err(MetadataError::Invalid(format!("condition must be one of {}", ALERT_OPS.join(" "))));
        }
        if !self.threshold.is_finite() {
            return Err(MetadataError::Invalid("threshold must be a finite number".into()));
        }
        if !(1..=31 * 86_400).contains(&self.window_secs) {
            return Err(MetadataError::Invalid("window must be between 1 second and 31 days".into()));
        }
        if !(10..=86_400).contains(&self.interval_secs) {
            return Err(MetadataError::Invalid("evaluation interval must be between 10 seconds and 1 day".into()));
        }
        if let Some(u) = &self.webhook_url
            && !(u.is_empty() || u.starts_with("http://") || u.starts_with("https://"))
        {
            return Err(MetadataError::Invalid("webhook URL must start with http:// or https://".into()));
        }
        Ok(())
    }
}

fn panel_from_row(r: &Row) -> rusqlite::Result<Panel> {
    let kind: String = r.get("kind")?;
    let config: String = r.get("config")?;
    Ok(Panel {
        id: r.get("id")?,
        dashboard_id: r.get("dashboard_id")?,
        title: r.get("title")?,
        kind: PanelKind::parse(&kind).unwrap_or(PanelKind::LogCount),
        config: serde_json::from_str(&config).unwrap_or(serde_json::Value::Null),
        position: r.get("position")?,
        width: r.get("width")?,
        height: r.get("height")?,
    })
}

fn alert_from_row(r: &Row) -> rusqlite::Result<Alert> {
    let status: Option<String> = r.get("state")?;
    Ok(Alert {
        id: r.get("id")?,
        def: AlertInput {
            name: r.get("name")?,
            query: r.get("query")?,
            op: r.get("condition_op")?,
            threshold: r.get("threshold")?,
            window_secs: r.get("window_secs")?,
            interval_secs: r.get("interval_secs")?,
            webhook_url: r.get("webhook_url")?,
            enabled: r.get::<_, i64>("enabled")? != 0,
        },
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        state: AlertState {
            status: status.as_deref().and_then(AlertStatus::parse).unwrap_or(AlertStatus::Ok),
            last_value: r.get("last_value")?,
            last_evaluated_at: r.get("last_evaluated_at")?,
            last_transition_at: r.get("last_transition_at")?,
            last_error: r.get("last_error")?,
            last_notification_at: r.get("last_notification_at")?,
            last_notification_error: r.get("last_notification_error")?,
        },
    })
}

/// An SQLite `file:` URI for `path` (forward slashes, a leading `/` before
/// a Windows drive letter, and `%`, `?` and `#` escaped).
fn sqlite_uri(path: &Path, immutable: bool) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file:");
    if p.as_bytes().get(1) == Some(&b':') {
        uri.push('/');
    }
    for c in p.chars() {
        match c {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            c => uri.push(c),
        }
    }
    if immutable {
        uri.push_str("?immutable=1");
    }
    uri
}

const ALERT_SELECT: &str = "SELECT a.*, s.state, s.last_value, s.last_evaluated_at, s.last_transition_at, \
     s.last_error, s.last_notification_at, s.last_notification_error \
     FROM alerts a LEFT JOIN alert_state s ON s.alert_id = a.id";

impl Metadata {
    /// Open the database, or create it with its schema if it has none.
    /// A database of another layout is refused unchanged.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| MetadataError::Invalid(format!("create metadata dir: {e}")))?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// The identity of the database at `path`, read without changing
    /// anything: no journal or lock files are created when none exist.
    pub fn inspect(path: &Path) -> Result<Identity> {
        use rusqlite::OpenFlags;
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        // `immutable` reads the file without locking or creating `-wal` and
        // `-shm` files; it would miss an unmerged `-wal`, so it is only used
        // when there is none (a cleanly closed database).
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let immutable = !Path::new(&wal).exists();
        let conn = Connection::open_with_flags(sqlite_uri(path, immutable), flags)?;
        identify(&conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        Self::init_with(conn, schema::SCHEMA)
    }

    fn init_with(mut conn: Connection, schema: &str) -> Result<Self> {
        // Before anything is written, even a journal mode.
        let identity = identify(&conn)?;
        if let Identity::Foreign(found) = identity {
            return Err(MetadataError::ForeignLayout { found });
        }
        // WAL for concurrent readers and crash safety; foreign keys are
        // off by default in SQLite and must be enabled per connection.
        // FULL syncs the WAL at every commit, so a write that returned
        // (an API key and the ingestion latch, a revoke, a logout) survives
        // a power failure or OS crash too; NORMAL could roll it back.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        if identity == Identity::Empty {
            Self::create(&mut conn, schema).map_err(MetadataError::Create)?;
            tracing::info!(layout = telemetry::LAYOUT, schema = SCHEMA_VERSION, "created metadata database");
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// The schema and the identity, in one transaction: an interruption (a
    /// failed statement, a crash, a full disk) leaves the database empty,
    /// and the next open creates it again.
    fn create(conn: &mut Connection, schema: &str) -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        tx.execute_batch(schema)?;
        tx.execute(
            "INSERT INTO vyrtel_layout (id, layout, schema_version, created_at) VALUES (1, ?1, ?2, ?3)",
            params![telemetry::LAYOUT, SCHEMA_VERSION, now_ms()],
        )?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.commit()
    }

    pub fn schema_version(&self) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row("SELECT schema_version FROM vyrtel_layout WHERE id = 1", [], |r| r.get(0))?)
    }

    // ---- settings -------------------------------------------------------

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let c = self.conn.lock().unwrap();
        Ok(c.prepare_cached("SELECT value FROM settings WHERE key = ?1")?.query_row([key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.prepare_cached(
            "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )?
        .execute(params![key, value, now_ms()])?;
        Ok(())
    }

    // ---- API keys -------------------------------------------------------

    /// Store a new key that never expires: [`Metadata::create_api_key_until`].
    pub fn create_api_key(&self, name: &str, prefix: &str, key_hash: &str, scope: &str) -> Result<ApiKey> {
        self.create_api_key_until(name, prefix, key_hash, scope, None)
    }

    /// Store a new key. Only the hash is persisted; the caller shows the
    /// plaintext to the user once. An ingest-scoped key also latches
    /// ingestion protection (see [`Metadata::ingest_latched`]) in the same
    /// transaction, so a key that was stored has always latched it.
    ///
    /// The name is trimmed and must be unique among active keys, ignoring
    /// ASCII case ([`MetadataError::Conflict`] otherwise); a revoked key's
    /// name can be used again. `expires_at` (ms since the epoch) must be in
    /// the future. Nothing is stored when any check fails.
    pub fn create_api_key_until(
        &self,
        name: &str,
        prefix: &str,
        key_hash: &str,
        scope: &str,
        expires_at: Option<i64>,
    ) -> Result<ApiKey> {
        let name = normalize_api_key_name(name)?;
        if !API_KEY_SCOPES.contains(&scope) {
            return Err(MetadataError::Invalid("scope must be 'ingest', 'query' or 'admin'".into()));
        }
        let now = now_ms();
        check_api_key_expiry(expires_at, now)?;
        let mut c = self.conn.lock().unwrap();
        // Immediate: the name check and the insert are one write, which waits
        // for the write lock instead of failing on another writer.
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let taken: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM api_keys WHERE revoked_at IS NULL AND name = ?1 COLLATE NOCASE)",
            [&name],
            |r| r.get(0),
        )?;
        if taken {
            return Err(MetadataError::Conflict(format!(
                "an active API key is already named {name:?}; choose another name, or rotate that key"
            )));
        }
        let new = NewKey { name: &name, prefix, key_hash, scope, expires_at, rotated_from: None };
        let key = insert_api_key(&tx, new, now)?;
        tx.commit()?;
        Ok(key)
    }

    /// Store a replacement for active key `id`: same name and scope, a new
    /// secret, and `expires_at` (ms since the epoch, or `None` for never;
    /// it must be in the future). Key `id` is left active: both are
    /// accepted until each is revoked, so clients can move over. Fails,
    /// storing nothing, with [`MetadataError::NotFound`] if `id` is not an
    /// active key, and with [`MetadataError::Conflict`] if it has expired:
    /// an expiry is final, so rotating cannot turn an expired key into a
    /// usable one.
    pub fn rotate_api_key(&self, id: i64, prefix: &str, key_hash: &str, expires_at: Option<i64>) -> Result<ApiKey> {
        let now = now_ms();
        check_api_key_expiry(expires_at, now)?;
        let mut c = self.conn.lock().unwrap();
        // Immediate: the check and the insert are one write, which waits for
        // the write lock instead of failing on another writer.
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (name, scope, expired): (String, String, Option<i64>) = tx
            .query_row("SELECT name, scope, expires_at FROM api_keys WHERE id = ?1 AND revoked_at IS NULL", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?
            .ok_or(MetadataError::NotFound)?;
        if expired.is_some_and(|at| at <= now) {
            return Err(MetadataError::Conflict(format!(
                "API key {id} has expired and cannot be rotated; revoke it and create a new key"
            )));
        }
        let new = NewKey { name: &name, prefix, key_hash, scope: &scope, expires_at, rotated_from: Some(id) };
        let key = insert_api_key(&tx, new, now)?;
        tx.commit()?;
        Ok(key)
    }

    /// Every active (not revoked) key, expired ones included, by id.
    pub fn list_api_keys(&self) -> Result<Vec<ApiKey>> {
        let c = self.conn.lock().unwrap();
        let mut st =
            c.prepare_cached(&format!("SELECT {API_KEY_COLUMNS} FROM api_keys WHERE revoked_at IS NULL ORDER BY id"))?;
        let rows = st.query_map([], api_key_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every active (not revoked) key, expired ones included.
    pub fn active_api_keys(&self) -> Result<Vec<ActiveApiKey>> {
        let c = self.conn.lock().unwrap();
        let mut st =
            c.prepare_cached("SELECT id, key_hash, scope, expires_at FROM api_keys WHERE revoked_at IS NULL")?;
        let rows = st.query_map([], |r| {
            Ok(ActiveApiKey { id: r.get(0)?, key_hash: r.get(1)?, scope: r.get(2)?, expires_at: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn touch_api_key(&self, id: i64) -> Result<()> {
        self.touch_api_keys(&[id]).map(|_| ())
    }

    /// Set `last_used_at` to now for every active key in `ids`, in one
    /// transaction. Revoked keys are left as they are. Returns how many
    /// keys were updated.
    pub fn touch_api_keys(&self, ids: &[i64]) -> Result<usize> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let now = now_ms();
        let mut n = 0;
        {
            let mut st =
                tx.prepare_cached("UPDATE api_keys SET last_used_at = ?2 WHERE id = ?1 AND revoked_at IS NULL")?;
            for id in ids {
                n += st.execute(params![id, now])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn revoke_api_key(&self, id: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        let n = c
            .prepare_cached("UPDATE api_keys SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL")?
            .execute(params![id, now_ms()])?;
        if n == 0 {
            return Err(MetadataError::NotFound);
        }
        Ok(())
    }

    pub fn count_api_keys(&self) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row("SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL", [], |r| r.get(0))?)
    }

    /// The ingestion authentication mode; [`IngestAuthMode::Automatic`] when
    /// none was ever set. Fails closed: a value this build does not write is
    /// refused, never read as a default.
    pub fn ingest_auth_mode(&self) -> Result<IngestAuthMode> {
        let Some(value) = self.get_setting(INGEST_AUTH_MODE)? else {
            return Ok(IngestAuthMode::Automatic);
        };
        IngestAuthMode::parse(&value).ok_or_else(|| {
            MetadataError::Invalid(format!(
                "the metadata database's ingestion mode setting ({INGEST_AUTH_MODE}) is {value:?}, which this \
                 Vyrtel does not recognise, so it cannot tell whether ingestion must be protected. Restore the \
                 metadata database from a backup, or set the value to 'automatic', 'required' or 'open' (see \
                 docs/configuration.md#ingestion-authentication)"
            ))
        })
    }

    /// Store the ingestion authentication mode. Leaves the latch as it is.
    pub fn set_ingest_auth_mode(&self, mode: IngestAuthMode) -> Result<()> {
        self.set_setting(INGEST_AUTH_MODE, mode.as_str())
    }

    /// Whether ingestion protection is latched: the first ingest-scoped key
    /// ever created latches it, and nothing clears it (revoking keys does
    /// not). Read at startup.
    ///
    /// Fails closed, and never writes. A latch with any value but
    /// [`INGEST_LATCHED`] is refused, never read as unlatched. So is a
    /// missing latch in a database that has held an ingest-scoped key
    /// (active or revoked): every such key was stored with its latch, so the
    /// row was removed and the database cannot be trusted to say ingestion
    /// is open.
    pub fn ingest_latched(&self) -> Result<bool> {
        let c = self.conn.lock().unwrap();
        let value: Option<String> = c
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?
            .query_row([INGEST_PROTECTION], |r| r.get(0))
            .optional()?;
        let refuse = |found: String| {
            MetadataError::Invalid(format!(
                "the metadata database's ingestion protection setting ({INGEST_PROTECTION}) is {found}, so it \
                 cannot tell whether ingestion must stay protected. Restore the metadata database from a backup, or \
                 set the value to '{INGEST_LATCHED}' to keep ingestion protected (see \
                 docs/configuration.md#ingestion-authentication)"
            ))
        };
        match value {
            Some(v) if v == INGEST_LATCHED => Ok(true),
            Some(other) => Err(refuse(format!("{other:?}, which this Vyrtel does not recognise"))),
            None => {
                let had_ingest_key: bool =
                    c.query_row("SELECT EXISTS (SELECT 1 FROM api_keys WHERE scope = 'ingest')", [], |r| r.get(0))?;
                if had_ingest_key {
                    Err(refuse("missing although ingest-scoped keys were created".into()))
                } else {
                    Ok(false)
                }
            }
        }
    }

    // ---- users & sessions ---------------------------------------------

    /// Create a local account. The username is stored case-normalized
    /// ([`normalize_username`]); one that exists in any case is a
    /// [`MetadataError::Conflict`]. `display_name` defaults to the username
    /// as given. `password_hash` must be an Argon2id PHC string, never a
    /// password.
    pub fn create_user(&self, username: &str, display_name: Option<&str>, password_hash: &str) -> Result<User> {
        let key = normalize_username(username)?;
        let display = normalize_display_name(display_name.unwrap_or(username))?;
        check_password_hash(password_hash)?;
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let taken: bool =
            tx.query_row("SELECT EXISTS (SELECT 1 FROM users WHERE username = ?1)", [&key], |r| r.get(0))?;
        if taken {
            return Err(MetadataError::Conflict(format!("username {key} is already taken")));
        }
        let now = now_ms();
        tx.prepare_cached(
            "INSERT INTO users (username, display_name, active, password_hash, created_at, updated_at) \
             VALUES (?1, ?2, 1, ?3, ?4, ?4)",
        )?
        .execute(params![key, display, password_hash, now])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(User {
            id,
            username: key,
            display_name: display,
            active: true,
            created_at: now,
            updated_at: now,
            password_hash: password_hash.to_string(),
        })
    }

    /// Set the password hash of `username`, creating the account (active,
    /// display name as given) if there is none. An existing account keeps
    /// its id, display name, active state and sessions.
    pub fn upsert_user(&self, username: &str, password_hash: &str) -> Result<i64> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let id = set_password(&tx, username, password_hash)?;
        tx.commit()?;
        Ok(id)
    }

    /// Set the password hash of `username` (creating the user if needed)
    /// and end all of that user's sessions, in one transaction, so no
    /// session issued under the old password outlives the change. Returns
    /// the user id and how many sessions were ended.
    pub fn rotate_password(&self, username: &str, password_hash: &str) -> Result<(i64, usize)> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let id = set_password(&tx, username, password_hash)?;
        let ended = tx.prepare_cached("DELETE FROM sessions WHERE user_id = ?1")?.execute([id])?;
        tx.commit()?;
        Ok((id, ended))
    }

    /// The account `username` names, in any case. A username that could
    /// not be stored names none.
    pub fn find_user(&self, username: &str) -> Result<Option<User>> {
        let Ok(key) = normalize_username(username) else { return Ok(None) };
        let c = self.conn.lock().unwrap();
        Ok(c.prepare_cached(&format!("SELECT {USER_COLUMNS} FROM users WHERE username = ?1"))?
            .query_row([key], user_from_row)
            .optional()?)
    }

    pub fn find_user_by_id(&self, id: i64) -> Result<Option<User>> {
        let c = self.conn.lock().unwrap();
        Ok(c.prepare_cached(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"))?
            .query_row([id], user_from_row)
            .optional()?)
    }

    /// Activate or deactivate an account in SQLite. Deactivating ends its
    /// sessions in the same transaction. Returns the account's username and
    /// how many sessions were ended.
    ///
    /// This does not reach the server's session cache: call
    /// `server::auth::set_account_active` instead, which also drops the
    /// user's cached sessions so revocation is immediate.
    pub fn set_user_active(&self, id: i64, active: bool) -> Result<(String, usize)> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let username: Option<String> = tx
            .prepare_cached("UPDATE users SET active = ?2, updated_at = ?3 WHERE id = ?1 RETURNING username")?
            .query_row(params![id, active, now_ms()], |r| r.get(0))
            .optional()?;
        let Some(username) = username else { return Err(MetadataError::NotFound) };
        let ended =
            if active { 0 } else { tx.prepare_cached("DELETE FROM sessions WHERE user_id = ?1")?.execute([id])? };
        tx.commit()?;
        Ok((username, ended))
    }

    pub fn count_users(&self) -> Result<i64> {
        let c = self.conn.lock().unwrap();
        Ok(c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
    }

    /// Start a session of an active user, as [`Metadata::issue_session`]
    /// does without a password check or a replaced session.
    pub fn create_session(&self, token_hash: &str, user_id: i64, ttl_ms: i64) -> Result<Vec<String>> {
        self.issue_session(&NewSession { token_hash, user_id, verified_hash: None, ttl_ms, replaces: None })
    }

    /// Start a session in one transaction, so nothing committed before it
    /// can be undone by it:
    ///
    /// * the session `replaces` (the credential the client presented, by its
    ///   token's SHA-256) ends, whoever it belonged to: a login rotates it;
    /// * the session is created only if the user is active and, when
    ///   `verified_hash` is given, still has that password hash. Otherwise
    ///   nothing changes and it is [`MetadataError::NotFound`], so a login
    ///   that checked the password before a deactivation or password change
    ///   committed cannot start a session after it;
    /// * the user keeps at most [`MAX_SESSIONS_PER_USER`]: its expired
    ///   sessions end (not this one), then its oldest.
    ///
    /// The expiry is `ttl_ms` from now and never changes afterwards. Returns
    /// the hashes of every session it ended, for the server's session cache.
    pub fn issue_session(&self, s: &NewSession) -> Result<Vec<String>> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let now = now_ms();
        let mut ended: Vec<String> = Vec::new();
        if let Some(old) = s.replaces {
            let gone: Option<String> = tx
                .prepare_cached("DELETE FROM sessions WHERE token_hash = ?1 RETURNING token_hash")?
                .query_row([old], |r| r.get(0))
                .optional()?;
            ended.extend(gone);
        }
        let created = tx
            .prepare_cached(
                "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) \
                 SELECT ?1, id, ?3, ?4 FROM users WHERE id = ?2 AND active = 1 \
                 AND (?5 IS NULL OR password_hash = ?5)",
            )?
            .execute(params![s.token_hash, s.user_id, now, now.saturating_add(s.ttl_ms), s.verified_hash])?;
        if created == 0 {
            return Err(MetadataError::NotFound);
        }
        {
            let mut over = tx.prepare_cached(
                "DELETE FROM sessions WHERE user_id = ?1 AND token_hash <> ?4 \
                 AND (expires_at <= ?2 OR token_hash IN ( \
                 SELECT token_hash FROM sessions WHERE user_id = ?1 \
                 ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?3)) RETURNING token_hash",
            )?;
            let rows =
                over.query_map(params![s.user_id, now, MAX_SESSIONS_PER_USER as i64, s.token_hash], |r| r.get(0))?;
            for hash in rows {
                ended.push(hash?);
            }
        }
        tx.commit()?;
        Ok(ended)
    }

    /// Set account `id`'s password hash (a reset or a change) and end all of
    /// its sessions, in one transaction. Returns its username and how many
    /// sessions were ended.
    ///
    /// This does not reach the server's session cache: call
    /// `server::auth::set_account_password` instead.
    pub fn set_user_password(&self, id: i64, password_hash: &str) -> Result<(String, usize)> {
        check_password_hash(password_hash)?;
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let username: Option<String> = tx
            .prepare_cached("UPDATE users SET password_hash = ?2, updated_at = ?3 WHERE id = ?1 RETURNING username")?
            .query_row(params![id, password_hash, now_ms()], |r| r.get(0))
            .optional()?;
        let Some(username) = username else { return Err(MetadataError::NotFound) };
        let ended = tx.prepare_cached("DELETE FROM sessions WHERE user_id = ?1")?.execute([id])?;
        tx.commit()?;
        Ok((username, ended))
    }

    /// How many sessions account `id` has, expired ones included.
    pub fn count_sessions(&self, user_id: i64) -> Result<usize> {
        let c = self.conn.lock().unwrap();
        Ok(c.prepare_cached("SELECT COUNT(*) FROM sessions WHERE user_id = ?1")?
            .query_row([user_id], |r| r.get::<_, i64>(0))? as usize)
    }

    pub fn find_session(&self, token_hash: &str) -> Result<Option<User>> {
        self.find_session_timed(token_hash).map(|(found, _)| found.map(|(user, _)| user))
    }

    /// [`Metadata::find_session`] with the session's expiry (ms since the
    /// epoch), also returning how long it waited for the connection, so
    /// request authentication can tell waiting behind other metadata work
    /// from the lookup itself.
    #[allow(clippy::type_complexity)]
    pub fn find_session_timed(&self, token_hash: &str) -> Result<(Option<(User, i64)>, std::time::Duration)> {
        let waiting = std::time::Instant::now();
        let c = self.conn.lock().unwrap();
        let lock_wait = waiting.elapsed();
        let found = c
            .prepare_cached(
                "SELECT u.id, u.username, u.display_name, u.active, u.created_at, u.updated_at, u.password_hash, \
                 s.expires_at FROM sessions s JOIN users u ON u.id = s.user_id \
                 WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND u.active = 1",
            )?
            .query_row(params![token_hash, now_ms()], |r| Ok((user_from_row(r)?, r.get("expires_at")?)))
            .optional()?;
        Ok((found, lock_wait))
    }

    pub fn delete_session(&self, token_hash: &str) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.prepare_cached("DELETE FROM sessions WHERE token_hash = ?1")?.execute([token_hash])?;
        Ok(())
    }

    pub fn purge_expired_sessions(&self) -> Result<usize> {
        let c = self.conn.lock().unwrap();
        Ok(c.prepare_cached("DELETE FROM sessions WHERE expires_at <= ?1")?.execute([now_ms()])?)
    }

    // ---- dashboards -----------------------------------------------------

    pub fn create_dashboard(&self, name: &str, description: Option<&str>) -> Result<Dashboard> {
        validate_name(name, "dashboard")?;
        let c = self.conn.lock().unwrap();
        let now = now_ms();
        c.prepare_cached("INSERT INTO dashboards (name, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)")?
            .execute(params![name.trim(), description, now])?;
        let id = c.last_insert_rowid();
        drop(c);
        self.get_dashboard(id)
    }

    pub fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let ids: Vec<i64> = {
            let c = self.conn.lock().unwrap();
            let mut st = c.prepare_cached("SELECT id FROM dashboards ORDER BY name COLLATE NOCASE, id")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        ids.into_iter().map(|id| self.get_dashboard(id)).collect()
    }

    pub fn get_dashboard(&self, id: i64) -> Result<Dashboard> {
        let c = self.conn.lock().unwrap();
        let mut d = c
            .prepare_cached("SELECT id, name, description, created_at, updated_at FROM dashboards WHERE id = ?1")?
            .query_row([id], |r| {
                Ok(Dashboard {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    description: r.get(2)?,
                    created_at: r.get(3)?,
                    updated_at: r.get(4)?,
                    panels: Vec::new(),
                })
            })
            .optional()?
            .ok_or(MetadataError::NotFound)?;
        let mut st =
            c.prepare_cached("SELECT * FROM dashboard_panels WHERE dashboard_id = ?1 ORDER BY position, id")?;
        d.panels = st.query_map([id], panel_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(d)
    }

    pub fn update_dashboard(&self, id: i64, name: &str, description: Option<&str>) -> Result<Dashboard> {
        validate_name(name, "dashboard")?;
        {
            let c = self.conn.lock().unwrap();
            let n = c
                .prepare_cached("UPDATE dashboards SET name = ?2, description = ?3, updated_at = ?4 WHERE id = ?1")?
                .execute(params![id, name.trim(), description, now_ms()])?;
            if n == 0 {
                return Err(MetadataError::NotFound);
            }
        }
        self.get_dashboard(id)
    }

    pub fn delete_dashboard(&self, id: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        // Panels go with it (ON DELETE CASCADE).
        if c.prepare_cached("DELETE FROM dashboards WHERE id = ?1")?.execute([id])? == 0 {
            return Err(MetadataError::NotFound);
        }
        Ok(())
    }

    pub fn add_panel(&self, dashboard_id: i64, p: &PanelInput) -> Result<Panel> {
        validate_name(&p.title, "panel")?;
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let exists: bool = tx
            .query_row("SELECT 1 FROM dashboards WHERE id = ?1", [dashboard_id], |_| Ok(true))
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(MetadataError::NotFound);
        }
        let position = match p.position {
            Some(pos) => pos,
            None => tx.query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM dashboard_panels WHERE dashboard_id = ?1",
                [dashboard_id],
                |r| r.get(0),
            )?,
        };
        let now = now_ms();
        tx.execute(
            "INSERT INTO dashboard_panels (dashboard_id, title, kind, config, position, width, height, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![
                dashboard_id,
                p.title.trim(),
                p.kind.as_str(),
                p.config.to_string(),
                position,
                p.width.unwrap_or(6).clamp(1, 12),
                p.height.unwrap_or(4).clamp(1, 12),
                now
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute("UPDATE dashboards SET updated_at = ?2 WHERE id = ?1", params![dashboard_id, now])?;
        let panel = tx.query_row("SELECT * FROM dashboard_panels WHERE id = ?1", [id], panel_from_row)?;
        tx.commit()?;
        Ok(panel)
    }

    pub fn update_panel(&self, dashboard_id: i64, panel_id: i64, p: &PanelInput) -> Result<Panel> {
        validate_name(&p.title, "panel")?;
        let c = self.conn.lock().unwrap();
        let now = now_ms();
        let n = c
            .prepare_cached(
                "UPDATE dashboard_panels SET title = ?3, kind = ?4, config = ?5, \
                 position = COALESCE(?6, position), width = COALESCE(?7, width), height = COALESCE(?8, height), updated_at = ?9 \
                 WHERE id = ?1 AND dashboard_id = ?2",
            )?
            .execute(params![
                panel_id,
                dashboard_id,
                p.title.trim(),
                p.kind.as_str(),
                p.config.to_string(),
                p.position,
                p.width.map(|w| w.clamp(1, 12)),
                p.height.map(|h| h.clamp(1, 12)),
                now
            ])?;
        if n == 0 {
            return Err(MetadataError::NotFound);
        }
        c.execute("UPDATE dashboards SET updated_at = ?2 WHERE id = ?1", params![dashboard_id, now])?;
        Ok(c.query_row("SELECT * FROM dashboard_panels WHERE id = ?1", [panel_id], panel_from_row)?)
    }

    pub fn delete_panel(&self, dashboard_id: i64, panel_id: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        let n = c
            .prepare_cached("DELETE FROM dashboard_panels WHERE id = ?1 AND dashboard_id = ?2")?
            .execute(params![panel_id, dashboard_id])?;
        if n == 0 {
            return Err(MetadataError::NotFound);
        }
        Ok(())
    }

    // ---- saved queries --------------------------------------------------

    pub fn create_saved_query(&self, name: &str, signal: &str, query: &str) -> Result<SavedQuery> {
        validate_name(name, "saved query")?;
        if !matches!(signal, "logs" | "traces" | "metrics") {
            return Err(MetadataError::Invalid("signal must be logs, traces or metrics".into()));
        }
        let c = self.conn.lock().unwrap();
        let now = now_ms();
        c.prepare_cached("INSERT INTO saved_queries (name, signal, query, created_at) VALUES (?1, ?2, ?3, ?4)")?
            .execute(params![name.trim(), signal, query, now])?;
        Ok(SavedQuery {
            id: c.last_insert_rowid(),
            name: name.trim().into(),
            signal: signal.into(),
            query: query.into(),
            created_at: now,
        })
    }

    pub fn list_saved_queries(&self) -> Result<Vec<SavedQuery>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare_cached(
            "SELECT id, name, signal, query, created_at FROM saved_queries ORDER BY name COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| {
            Ok(SavedQuery {
                id: r.get(0)?,
                name: r.get(1)?,
                signal: r.get(2)?,
                query: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_saved_query(&self, id: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        if c.prepare_cached("DELETE FROM saved_queries WHERE id = ?1")?.execute([id])? == 0 {
            return Err(MetadataError::NotFound);
        }
        Ok(())
    }

    // ---- alerts ---------------------------------------------------------

    pub fn create_alert(&self, a: &AlertInput) -> Result<Alert> {
        a.validate()?;
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let now = now_ms();
        tx.execute(
            "INSERT INTO alerts (name, query, condition_op, threshold, window_secs, interval_secs, webhook_url, enabled, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                a.name.trim(),
                a.query,
                a.op,
                a.threshold,
                a.window_secs,
                a.interval_secs,
                a.webhook_url.as_deref().filter(|u| !u.is_empty()),
                a.enabled as i64,
                now
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO alert_state (alert_id, state) VALUES (?1, 'OK')", [id])?;
        tx.commit()?;
        drop(c);
        self.get_alert(id)
    }

    pub fn update_alert(&self, id: i64, a: &AlertInput) -> Result<Alert> {
        a.validate()?;
        {
            let c = self.conn.lock().unwrap();
            let n = c
                .prepare_cached(
                    "UPDATE alerts SET name = ?2, query = ?3, condition_op = ?4, threshold = ?5, window_secs = ?6, \
                     interval_secs = ?7, webhook_url = ?8, enabled = ?9, updated_at = ?10 WHERE id = ?1",
                )?
                .execute(params![
                    id,
                    a.name.trim(),
                    a.query,
                    a.op,
                    a.threshold,
                    a.window_secs,
                    a.interval_secs,
                    a.webhook_url.as_deref().filter(|u| !u.is_empty()),
                    a.enabled as i64,
                    now_ms()
                ])?;
            if n == 0 {
                return Err(MetadataError::NotFound);
            }
        }
        self.get_alert(id)
    }

    pub fn get_alert(&self, id: i64) -> Result<Alert> {
        let c = self.conn.lock().unwrap();
        c.prepare_cached(&format!("{ALERT_SELECT} WHERE a.id = ?1"))?
            .query_row([id], alert_from_row)
            .optional()?
            .ok_or(MetadataError::NotFound)
    }

    pub fn list_alerts(&self) -> Result<Vec<Alert>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare_cached(&format!("{ALERT_SELECT} ORDER BY a.name COLLATE NOCASE, a.id"))?;
        let rows = st.query_map([], alert_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_alert(&self, id: i64) -> Result<()> {
        let c = self.conn.lock().unwrap();
        if c.prepare_cached("DELETE FROM alerts WHERE id = ?1")?.execute([id])? == 0 {
            return Err(MetadataError::NotFound);
        }
        Ok(())
    }

    /// Persist an evaluation result; records a transition when the status
    /// changed. Returns the previous status.
    pub fn record_alert_evaluation(
        &self,
        id: i64,
        status: AlertStatus,
        value: Option<f64>,
        error: Option<&str>,
    ) -> Result<AlertStatus> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let prev: String = tx
            .query_row("SELECT state FROM alert_state WHERE alert_id = ?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(MetadataError::NotFound)?;
        let prev = AlertStatus::parse(&prev).unwrap_or(AlertStatus::Ok);
        let now = now_ms();
        if prev != status {
            tx.execute(
                "UPDATE alert_state SET state = ?2, last_value = ?3, last_evaluated_at = ?4, last_transition_at = ?4, last_error = ?5 WHERE alert_id = ?1",
                params![id, status.as_str(), value, now, error],
            )?;
            tx.execute(
                "INSERT INTO alert_history (alert_id, from_state, to_state, value, message, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, prev.as_str(), status.as_str(), value, error, now],
            )?;
        } else {
            tx.execute(
                "UPDATE alert_state SET last_value = ?2, last_evaluated_at = ?3, last_error = ?4 WHERE alert_id = ?1",
                params![id, value, now, error],
            )?;
        }
        tx.commit()?;
        Ok(prev)
    }

    pub fn record_alert_notification(&self, id: i64, error: Option<&str>) -> Result<()> {
        let c = self.conn.lock().unwrap();
        c.prepare_cached(
            "UPDATE alert_state SET last_notification_at = ?2, last_notification_error = ?3 WHERE alert_id = ?1",
        )?
        .execute(params![id, now_ms(), error])?;
        Ok(())
    }

    pub fn alert_history(&self, id: i64, limit: usize) -> Result<Vec<AlertTransition>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare_cached(
            "SELECT from_state, to_state, value, message, at FROM alert_history WHERE alert_id = ?1 ORDER BY at DESC, id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![id, limit as i64], |r| {
            Ok(AlertTransition { from: r.get(0)?, to: r.get(1)?, value: r.get(2)?, message: r.get(3)?, at: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- query statistics ----------------------------------------------

    /// Add a batch of increments to the persisted totals.
    pub fn add_query_stats(&self, rows: &[StoredQueryStats]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO query_stats (signal, field, index_kind, queries, bytes_read, segments_scanned, segments_skipped, total_ms, last_seen) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT(signal, field) DO UPDATE SET \
                   index_kind = excluded.index_kind, \
                   queries = queries + excluded.queries, \
                   bytes_read = bytes_read + excluded.bytes_read, \
                   segments_scanned = segments_scanned + excluded.segments_scanned, \
                   segments_skipped = segments_skipped + excluded.segments_skipped, \
                   total_ms = total_ms + excluded.total_ms, \
                   last_seen = excluded.last_seen",
            )?;
            for r in rows {
                st.execute(params![
                    r.signal,
                    r.field,
                    r.index,
                    r.queries as i64,
                    r.bytes_read as i64,
                    r.segments_scanned as i64,
                    r.segments_skipped as i64,
                    r.total_ms,
                    r.last_seen
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn query_stats(&self) -> Result<Vec<StoredQueryStats>> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare_cached(
            "SELECT signal, field, index_kind, queries, bytes_read, segments_scanned, segments_skipped, total_ms, last_seen \
             FROM query_stats ORDER BY queries DESC, field",
        )?;
        let rows = st.query_map([], |r| {
            Ok(StoredQueryStats {
                signal: r.get(0)?,
                field: r.get(1)?,
                index: r.get(2)?,
                queries: r.get::<_, i64>(3)? as u64,
                bytes_read: r.get::<_, i64>(4)? as u64,
                segments_scanned: r.get::<_, i64>(5)? as u64,
                segments_skipped: r.get::<_, i64>(6)? as u64,
                total_ms: r.get(7)?,
                last_seen: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Refuse anything but a well-formed Argon2id PHC string (a known version,
/// valid parameters, a salt and an output), so neither a plaintext password
/// nor a malformed verifier that could never match is stored.
fn check_password_hash(hash: &str) -> Result<()> {
    use argon2::password_hash::phc::PasswordHash;
    let well_formed = || -> Option<()> {
        let parsed = PasswordHash::new(hash).ok()?;
        if parsed.algorithm != argon2::Algorithm::Argon2id.ident() {
            return None;
        }
        parsed.salt.as_ref()?;
        parsed.hash.as_ref()?;
        if let Some(v) = parsed.version {
            argon2::Version::try_from(v).ok()?;
        }
        argon2::Params::try_from(&parsed).ok()?;
        Some(())
    };
    well_formed().ok_or_else(|| MetadataError::Invalid("a password must be stored as an Argon2id hash".into()))
}

/// Set `username`'s password hash in `tx`, creating the account if there is
/// none. Returns its id.
fn set_password(tx: &rusqlite::Transaction, username: &str, password_hash: &str) -> Result<i64> {
    let key = normalize_username(username)?;
    let display = normalize_display_name(username)?;
    check_password_hash(password_hash)?;
    let now = now_ms();
    tx.prepare_cached(
        "INSERT INTO users (username, display_name, active, password_hash, created_at, updated_at) \
         VALUES (?1, ?2, 1, ?3, ?4, ?4) \
         ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash, updated_at = excluded.updated_at",
    )?
    .execute(params![key, display, password_hash, now])?;
    Ok(tx.query_row("SELECT id FROM users WHERE username = ?1", [key], |r| r.get(0))?)
}

fn validate_name(name: &str, what: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() {
        return Err(MetadataError::Invalid(format!("{what} name is required")));
    }
    if n.chars().count() > 200 {
        return Err(MetadataError::Invalid(format!("{what} name is too long")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(path: &Path) -> Vec<String> {
        let c = Connection::open(path).unwrap();
        let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    }

    /// The schema, then a statement that fails: creation stops part-way.
    fn failing_schema() -> String {
        format!("{}\nINSERT INTO missing VALUES (1);", schema::SCHEMA)
    }

    #[test]
    fn an_interrupted_creation_leaves_nothing_and_is_redone() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metadata.db");
        match Metadata::init_with(Connection::open(&path).unwrap(), &failing_schema()) {
            Err(e @ MetadataError::Create(_)) => {
                assert!(e.to_string().starts_with("creating the metadata schema failed (nothing was created): "))
            }
            Err(e) => panic!("expected Create, got {e}"),
            Ok(_) => panic!("creation must fail"),
        }
        assert_eq!(Metadata::inspect(&path).unwrap(), Identity::Empty);
        assert!(tables(&path).is_empty(), "nothing of the schema remains");

        let m = Metadata::open(&path).unwrap();
        assert_eq!(m.schema_version().unwrap(), SCHEMA_VERSION);
        drop(m);
        assert_eq!(Metadata::inspect(&path).unwrap(), Identity::Current);
        let c = Connection::open(&path).unwrap();
        let app_id: i32 = c.query_row("PRAGMA application_id", [], |r| r.get(0)).unwrap();
        assert_eq!(app_id, APPLICATION_ID);
        let layout: String = c.query_row("SELECT layout FROM vyrtel_layout", [], |r| r.get(0)).unwrap();
        assert_eq!(layout, telemetry::LAYOUT);
    }

    // AUTH-250 review: an acknowledged write (the ingestion latch above
    // all) must survive power loss, so every commit syncs the WAL. A
    // power cut cannot be simulated here; this pins the setting that
    // gives the guarantee (SQLite: WAL + synchronous=FULL is durable).
    #[test]
    fn commits_are_synced_before_they_return() {
        let d = tempfile::tempdir().unwrap();
        for path in [d.path().join("new.db"), d.path().join("new.db")] {
            let m = Metadata::open(&path).unwrap();
            let c = m.conn.lock().unwrap();
            let mode: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
            let sync: i64 = c.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
            assert_eq!((mode.as_str(), sync), ("wal", 2), "WAL with synchronous = FULL (2), new and reopened");
        }
    }
}
