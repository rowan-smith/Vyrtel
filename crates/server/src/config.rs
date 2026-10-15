//! Configuration: defaults ← TOML file ← `VYRTEL_*` environment ← CLI.
//!
//! Every option is documented in docs/configuration.md.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// A byte size such as `64MB`. Units are powers of 1024 (`KB`=`KiB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ByteSize(pub u64);

impl ByteSize {
    pub fn parse(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let n: f64 = num.parse().map_err(|_| format!("invalid size '{s}'"))?;
        let mult: u64 = match unit.trim().to_ascii_uppercase().as_str() {
            "" | "B" => 1,
            "K" | "KB" | "KIB" => 1 << 10,
            "M" | "MB" | "MIB" => 1 << 20,
            "G" | "GB" | "GIB" => 1 << 30,
            "T" | "TB" | "TIB" => 1 << 40,
            _ => return Err(format!("invalid size unit in '{s}' (use B, KB, MB, GB)")),
        };
        if !n.is_finite() || n < 0.0 {
            return Err(format!("invalid size '{s}'"));
        }
        Ok(ByteSize((n * mult as f64) as u64))
    }

    pub fn bytes(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ByteSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&human_bytes(self.0))
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v.fract() == 0.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(ByteSize(n)),
            Raw::S(s) => ByteSize::parse(&s).map_err(de::Error::custom),
        }
    }
}

/// Parse durations like `500ms`, `30s`, `5m`, `1h30m`, `30d`, `2w`.
/// A bare number means seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("empty duration".into());
    }
    if let Ok(secs) = t.parse::<u64>() {
        return Ok(Duration::from_secs(secs));
    }
    let mut total = Duration::ZERO;
    let mut rest = t;
    while !rest.is_empty() {
        let num_len = rest.find(|c: char| !c.is_ascii_digit()).ok_or_else(|| format!("missing unit in '{s}'"))?;
        if num_len == 0 {
            return Err(format!("invalid duration '{s}'"));
        }
        let n: u64 = rest[..num_len].parse().map_err(|_| format!("invalid duration '{s}'"))?;
        rest = &rest[num_len..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        rest = &rest[unit_len..];
        let d = match unit {
            "ms" => Duration::from_millis(n),
            "s" => Duration::from_secs(n),
            "m" => Duration::from_secs(n * 60),
            "h" => Duration::from_secs(n * 3600),
            "d" => Duration::from_secs(n * 86_400),
            "w" => Duration::from_secs(n * 7 * 86_400),
            _ => return Err(format!("invalid duration unit '{unit}' in '{s}' (use ms, s, m, h, d, w)")),
        };
        total += d;
    }
    Ok(total)
}

pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if d.subsec_millis() != 0 && s < 60 {
        return format!("{}ms", d.as_millis());
    }
    for (unit, secs) in [("w", 604_800), ("d", 86_400), ("h", 3600), ("m", 60)] {
        if s >= secs && s.is_multiple_of(secs) {
            return format!("{}{unit}", s / secs);
        }
    }
    format!("{s}s")
}

mod duration_serde {
    use super::*;

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(Duration::from_secs(n)),
            Raw::S(s) => parse_duration(&s).map_err(de::Error::custom),
        }
    }

    pub fn serialize<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_duration(*d))
    }
}

/// Retention: a duration, or `"forever"`/`"0"` to keep data indefinitely.
mod retention_serde {
    use super::*;

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(0) => Ok(None),
            Raw::N(n) => Ok(Some(Duration::from_secs(n))),
            Raw::S(s) => parse_retention(&s).map_err(de::Error::custom),
        }
    }

    pub fn serialize<S: serde::Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            None => s.serialize_str("forever"),
            Some(d) => s.serialize_str(&format_duration(*d)),
        }
    }
}

pub fn parse_retention(s: &str) -> Result<Option<Duration>, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "forever" | "off" | "none" | "0" | "" => Ok(None),
        _ => parse_duration(s).map(Some),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Durability {
    #[default]
    Normal,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: String,
    /// After a stop signal, how long accepted work (in-flight requests,
    /// queued writes, the final WAL flush) may take to drain before the
    /// process gives up, reports what is unfinished and exits with status 1.
    #[serde(with = "duration_serde")]
    pub shutdown_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        // Inside Docker's default 10 s stop grace period, so the diagnostic
        // is printed before the container is killed.
        Self { bind: "0.0.0.0:8080".into(), shutdown_timeout: Duration::from_secs(8) }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSection {
    pub path: PathBuf,
    /// Global memory budget, split between the consumers listed in
    /// docs/configuration.md.
    pub max_memory: ByteSize,
    /// Uncompressed size at which the active segment is sealed.
    pub segment_target_size: ByteSize,
    #[serde(with = "duration_serde")]
    pub segment_max_age: Duration,
    pub segment_max_events: u64,
    pub durability: Durability,
    #[serde(with = "duration_serde")]
    pub fsync_interval: Duration,
    pub compaction: bool,
    pub zstd_level: i32,
    pub block_events: usize,
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./data"),
            max_memory: ByteSize(512 << 20),
            segment_target_size: ByteSize(64 << 20),
            segment_max_age: Duration::from_secs(300),
            segment_max_events: 2_000_000,
            durability: Durability::Normal,
            fsync_interval: Duration::from_secs(1),
            compaction: true,
            zstd_level: 3,
            block_events: 1024,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSection {
    #[serde(with = "retention_serde")]
    pub logs: Option<Duration>,
    #[serde(with = "retention_serde")]
    pub traces: Option<Duration>,
    #[serde(with = "retention_serde")]
    pub metrics: Option<Duration>,
}

impl Default for RetentionSection {
    fn default() -> Self {
        Self {
            logs: Some(Duration::from_secs(30 * 86_400)),
            traces: Some(Duration::from_secs(14 * 86_400)),
            metrics: Some(Duration::from_secs(30 * 86_400)),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IngestSection {
    pub max_request_size: ByteSize,
    /// Events waiting to be written, per signal. Beyond this, 429.
    pub queue_capacity: usize,
    #[serde(with = "duration_serde")]
    pub ack_timeout: Duration,
    /// Ingest requests admitted at once, from reading the body until the
    /// write is acknowledged. Beyond this, 429. `0` derives it from the
    /// memory budget (see [`Config::ingest_admission`]).
    pub max_concurrent: usize,
}

impl Default for IngestSection {
    fn default() -> Self {
        Self {
            max_request_size: ByteSize(1 << 20),
            queue_capacity: 10_000,
            ack_timeout: Duration::from_secs(30),
            max_concurrent: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuerySection {
    #[serde(with = "duration_serde")]
    pub timeout: Duration,
    pub max_concurrent: usize,
    pub max_scan_bytes: ByteSize,
    pub max_results: usize,
    pub max_live_streams: usize,
}

impl Default for QuerySection {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_concurrent: 4,
            max_scan_bytes: ByteSize(4 << 30),
            max_results: 1000,
            max_live_streams: 64,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthSection {
    /// When false (the default), the UI and API are open. Enable before
    /// exposing Vyrtel beyond a trusted network.
    pub enabled: bool,
    pub admin_username: String,
    /// Plaintext admin password from config/env. Hashed into the metadata
    /// store at startup; never logged or returned by the API.
    #[serde(skip_serializing)]
    pub admin_password: Option<String>,
    #[serde(with = "duration_serde")]
    pub session_ttl: Duration,
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            enabled: false,
            admin_username: "admin".into(),
            admin_password: None,
            session_ttl: Duration::from_secs(7 * 86_400),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlertsSection {
    pub enabled: bool,
    #[serde(with = "duration_serde")]
    pub webhook_timeout: Duration,
}

impl Default for AlertsSection {
    fn default() -> Self {
        Self { enabled: true, webhook_timeout: Duration::from_secs(10) }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogSection {
    pub level: String,
    pub format: LogFormat,
}

impl Default for LogSection {
    fn default() -> Self {
        Self { level: "info".into(), format: LogFormat::Text }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageSection,
    pub retention: RetentionSection,
    pub ingest: IngestSection,
    pub query: QuerySection,
    pub auth: AuthSection,
    pub alerts: AlertsSection,
    pub log: LogSection,
}

/// Every supported environment variable, for docs and `--help`.
pub const ENV_VARS: &[&str] = &[
    "VYRTEL_SERVER_BIND",
    "VYRTEL_SERVER_SHUTDOWN_TIMEOUT",
    "VYRTEL_STORAGE_PATH",
    "VYRTEL_STORAGE_MAX_MEMORY",
    "VYRTEL_STORAGE_SEGMENT_TARGET_SIZE",
    "VYRTEL_STORAGE_SEGMENT_MAX_AGE",
    "VYRTEL_STORAGE_DURABILITY",
    "VYRTEL_STORAGE_FSYNC_INTERVAL",
    "VYRTEL_STORAGE_COMPACTION",
    "VYRTEL_RETENTION_LOGS",
    "VYRTEL_RETENTION_TRACES",
    "VYRTEL_RETENTION_METRICS",
    "VYRTEL_INGEST_MAX_REQUEST_SIZE",
    "VYRTEL_INGEST_QUEUE_CAPACITY",
    "VYRTEL_INGEST_MAX_CONCURRENT",
    "VYRTEL_QUERY_TIMEOUT",
    "VYRTEL_QUERY_MAX_CONCURRENT",
    "VYRTEL_AUTH_ENABLED",
    "VYRTEL_AUTH_ADMIN_USERNAME",
    "VYRTEL_AUTH_ADMIN_PASSWORD",
    "VYRTEL_ALERTS_ENABLED",
    "VYRTEL_LOG_LEVEL",
    "VYRTEL_LOG_FORMAT",
];

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("invalid boolean '{s}'")),
    }
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    pub fn load_file(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Apply `VYRTEL_<SECTION>_<KEY>` overrides.
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<(), String> {
        let wrap = |k: &str, r: Result<(), String>| r.map_err(|e| format!("{k}: {e}"));
        for key in ENV_VARS {
            let Some(v) = get(key) else { continue };
            let r: Result<(), String> = (|| {
                match *key {
                    "VYRTEL_SERVER_BIND" => self.server.bind = v.clone(),
                    "VYRTEL_SERVER_SHUTDOWN_TIMEOUT" => self.server.shutdown_timeout = parse_duration(&v)?,
                    "VYRTEL_STORAGE_PATH" => self.storage.path = PathBuf::from(&v),
                    "VYRTEL_STORAGE_MAX_MEMORY" => self.storage.max_memory = ByteSize::parse(&v)?,
                    "VYRTEL_STORAGE_SEGMENT_TARGET_SIZE" => self.storage.segment_target_size = ByteSize::parse(&v)?,
                    "VYRTEL_STORAGE_SEGMENT_MAX_AGE" => self.storage.segment_max_age = parse_duration(&v)?,
                    "VYRTEL_STORAGE_DURABILITY" => {
                        self.storage.durability = match v.trim().to_ascii_lowercase().as_str() {
                            "normal" => Durability::Normal,
                            "strict" => Durability::Strict,
                            _ => return Err("must be 'normal' or 'strict'".into()),
                        }
                    }
                    "VYRTEL_STORAGE_FSYNC_INTERVAL" => self.storage.fsync_interval = parse_duration(&v)?,
                    "VYRTEL_STORAGE_COMPACTION" => self.storage.compaction = parse_bool(&v)?,
                    "VYRTEL_RETENTION_LOGS" => self.retention.logs = parse_retention(&v)?,
                    "VYRTEL_RETENTION_TRACES" => self.retention.traces = parse_retention(&v)?,
                    "VYRTEL_RETENTION_METRICS" => self.retention.metrics = parse_retention(&v)?,
                    "VYRTEL_INGEST_MAX_REQUEST_SIZE" => self.ingest.max_request_size = ByteSize::parse(&v)?,
                    "VYRTEL_INGEST_QUEUE_CAPACITY" => {
                        self.ingest.queue_capacity = v.trim().parse().map_err(|_| "must be a number".to_string())?
                    }
                    "VYRTEL_INGEST_MAX_CONCURRENT" => {
                        self.ingest.max_concurrent = v.trim().parse().map_err(|_| "must be a number".to_string())?
                    }
                    "VYRTEL_QUERY_TIMEOUT" => self.query.timeout = parse_duration(&v)?,
                    "VYRTEL_QUERY_MAX_CONCURRENT" => {
                        self.query.max_concurrent = v.trim().parse().map_err(|_| "must be a number".to_string())?
                    }
                    "VYRTEL_AUTH_ENABLED" => self.auth.enabled = parse_bool(&v)?,
                    "VYRTEL_AUTH_ADMIN_USERNAME" => self.auth.admin_username = v.clone(),
                    "VYRTEL_AUTH_ADMIN_PASSWORD" => self.auth.admin_password = Some(v.clone()),
                    "VYRTEL_ALERTS_ENABLED" => self.alerts.enabled = parse_bool(&v)?,
                    "VYRTEL_LOG_LEVEL" => self.log.level = v.clone(),
                    "VYRTEL_LOG_FORMAT" => {
                        self.log.format = match v.trim().to_ascii_lowercase().as_str() {
                            "text" => LogFormat::Text,
                            "json" => LogFormat::Json,
                            _ => return Err("must be 'text' or 'json'".into()),
                        }
                    }
                    _ => {}
                }
                Ok(())
            })();
            wrap(key, r)?;
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.storage.max_memory.0 < 64 << 20 {
            return Err("storage.max_memory must be at least 64MB".into());
        }
        if self.storage.segment_target_size.0 < 64 << 10 {
            return Err("storage.segment_target_size must be at least 64KB".into());
        }
        if self.ingest.max_request_size.0 < 1024 {
            return Err("ingest.max_request_size must be at least 1KB".into());
        }
        if self.ingest.queue_capacity == 0 {
            return Err("ingest.queue_capacity must be positive".into());
        }
        if self.ingest.max_concurrent > INGEST_MAX_CONCURRENT_LIMIT {
            return Err(format!("ingest.max_concurrent must be at most {INGEST_MAX_CONCURRENT_LIMIT} (0 derives it)"));
        }
        let admission = self.ingest_admission();
        if admission.limit == 0 {
            return Err(admission.invalid_message());
        }
        if self.query.max_concurrent == 0 {
            return Err("query.max_concurrent must be positive".into());
        }
        if !(1..=22).contains(&self.storage.zstd_level) {
            return Err("storage.zstd_level must be between 1 and 22".into());
        }
        if self.storage.block_events == 0 {
            return Err("storage.block_events must be positive".into());
        }
        if self.server.shutdown_timeout.is_zero() {
            return Err("server.shutdown_timeout must be positive (e.g. 8s)".into());
        }
        if self.auth.admin_username.trim().is_empty() {
            return Err("auth.admin_username must not be empty".into());
        }
        self.server
            .bind
            .parse::<std::net::SocketAddr>()
            .map_err(|_| format!("server.bind '{}' is not a valid address (e.g. 0.0.0.0:8080)", self.server.bind))?;
        Ok(())
    }

    pub fn budgets(&self) -> MemoryBudgets {
        MemoryBudgets::from_limit(self.storage.max_memory.0, self.storage.segment_target_size.0)
    }

    /// How many ingest requests are admitted at once, and what they are
    /// estimated to hold. See [`IngestAdmission`].
    pub fn ingest_admission(&self) -> IngestAdmission {
        let budget = self.budgets().ingest;
        let max_request_bytes = self.ingest.max_request_size.0;
        let per_request = max_request_bytes.saturating_mul(INGEST_BYTES_PER_REQUEST_BYTE).max(1);
        let derived = self.ingest.max_concurrent == 0;
        let limit = if derived {
            let fits = budget.saturating_sub(INGEST_FIXED_OVERHEAD) / per_request;
            fits.min(INGEST_MAX_DERIVED_CONCURRENCY as u64) as usize
        } else {
            self.ingest.max_concurrent
        };
        let estimated_peak =
            per_request.saturating_mul(u64::try_from(limit).unwrap_or(u64::MAX)).saturating_add(INGEST_FIXED_OVERHEAD);
        IngestAdmission { limit, derived, budget, max_request_bytes, per_request, estimated_peak }
    }

    /// Effective ingest admission limit: `ingest.max_concurrent` when set,
    /// otherwise as many maximum-size requests as the ingest budget holds.
    pub fn ingest_max_concurrent(&self) -> usize {
        self.ingest_admission().limit
    }
}

/// Estimated heap held by one admitted request, per byte of
/// `ingest.max_request_size`: the read (and gzip-decoded) body, the parse
/// working set and the parsed batch, which stays admitted until its write
/// is acknowledged. Measured per format by the `amplification` test in
/// `tests/overload`: representative fixtures of every format and signal
/// peak at 7.0–16.0×, and the worst supported one (OTLP/JSON logs of
/// ~20-byte records) at 17.0× (17.3× with 1 MB fixtures). Event shapes
/// that amplify more, such as tiny native events, deeply nested properties
/// or tiny OTLP/protobuf records, are outside this estimate and are not
/// refused; see docs/configuration.md#ingest-memory.
pub const INGEST_BYTES_PER_REQUEST_BYTE: u64 = 20;

/// hyper's default HTTP/1 read buffer limit (`max_buf_size`).
const HTTP1_MAX_READ_BUFFER: u64 = 8192 + 4096 * 100;

/// Ingest memory not tied to admitted requests: rejected bodies being
/// drained. At most `MAX_DISCARDING` are read at once, each through its
/// connection's read buffer, so this is an upper bound by construction
/// (12.75 MB); the `rejected_body_drain_overhead_is_within_the_fixed_allowance`
/// test in `tests/overload` measures it. Read buffers of connections that
/// are not draining a rejected body are not included.
pub const INGEST_FIXED_OVERHEAD: u64 = crate::routes::ingest::MAX_DISCARDING as u64 * HTTP1_MAX_READ_BUFFER;

/// Upper bound on a derived admission limit.
pub const INGEST_MAX_DERIVED_CONCURRENCY: usize = 1024;

/// Largest explicit `ingest.max_concurrent` (the admission semaphore's
/// own limit).
pub const INGEST_MAX_CONCURRENT_LIMIT: usize = tokio::sync::Semaphore::MAX_PERMITS;

/// The effective ingest admission limit and the memory it is estimated to
/// need. Derived (`ingest.max_concurrent = 0`), the limit is
/// `(ingest budget − fixed overhead) ÷ (max_request_size ×
/// INGEST_BYTES_PER_REQUEST_BYTE)`, rounded down and capped at
/// [`INGEST_MAX_DERIVED_CONCURRENCY`]; zero means the configuration is
/// invalid. An explicit value is used as is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestAdmission {
    pub limit: usize,
    pub derived: bool,
    /// The ingest share of `storage.max_memory`.
    pub budget: u64,
    pub max_request_bytes: u64,
    /// Estimated bytes held by one maximum-size request.
    pub per_request: u64,
    /// `limit × per_request + fixed overhead`, saturating.
    pub estimated_peak: u64,
}

impl IngestAdmission {
    pub fn within_budget(&self) -> bool {
        self.estimated_peak <= self.budget
    }

    /// Why a derived limit of zero is invalid, and how to fix it.
    pub fn invalid_message(&self) -> String {
        let needed = self.per_request.saturating_add(INGEST_FIXED_OVERHEAD);
        // The ingest share is computed as max_memory / 100 * 15; both
        // suggestions are rounded so that they really fit (whole MB up, KB
        // down).
        let min_memory_mb = needed.div_ceil(15).saturating_mul(100).div_ceil(1 << 20);
        let max_request_kb = (self.budget.saturating_sub(INGEST_FIXED_OVERHEAD) / INGEST_BYTES_PER_REQUEST_BYTE) >> 10;
        let mut msg = format!(
            "ingest budget too small: one {} request needs an estimated {} ({INGEST_BYTES_PER_REQUEST_BYTE}× the request \
             size, plus {} fixed ingest overhead) but the ingest share of storage.max_memory is {}; increase \
             storage.max_memory to at least {min_memory_mb}MB",
            human_bytes(self.max_request_bytes),
            human_bytes(needed),
            human_bytes(INGEST_FIXED_OVERHEAD),
            human_bytes(self.budget),
        );
        if max_request_kb >= 1 {
            msg.push_str(&format!(", reduce ingest.max_request_size to at most {max_request_kb}KB"));
        }
        msg.push_str(", or set ingest.max_concurrent explicitly (see docs/configuration.md#ingest-memory)");
        msg
    }

    /// A startup warning when an explicit `ingest.max_concurrent` is
    /// estimated to exceed the ingest budget. `None` when derived or within
    /// budget.
    pub fn warning(&self) -> Option<String> {
        if self.derived || self.within_budget() {
            return None;
        }
        Some(format!(
            "ingest.max_concurrent = {} with ingest.max_request_size = {} may need an estimated {} of ingest memory \
             under load, more than the {} ingest budget; the limit is honoured. Lower ingest.max_concurrent or \
             ingest.max_request_size, raise storage.max_memory, or set ingest.max_concurrent = 0 to derive it \
             (see docs/configuration.md#ingest-memory)",
            self.limit,
            human_bytes(self.max_request_bytes),
            human_bytes(self.estimated_peak),
            human_bytes(self.budget),
        ))
    }
}

/// How the global memory limit is split between consumers. Each budget is
/// enforced by the component that owns it (see docs/configuration.md).
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBudgets {
    pub total: u64,
    /// Active + frozen in-memory segments, all signals.
    pub active_segments: u64,
    /// Effective seal threshold per signal (logs, traces, metrics).
    pub segment_target: [u64; 3],
    pub index_cache: u64,
    pub ingest: u64,
    pub queries: u64,
    pub background: u64,
}

impl MemoryBudgets {
    pub fn from_limit(total: u64, configured_target: u64) -> Self {
        let pct = |p: u64| total / 100 * p;
        let active = pct(40);
        // Each signal gets a share; active + one frozen buffer must fit in
        // it, so the seal threshold is at most half the share.
        let shares = [50, 30, 20];
        let segment_target = shares.map(|s| configured_target.min(active / 100 * s / 2).max(64 << 10));
        Self {
            total,
            active_segments: active,
            segment_target,
            index_cache: pct(15),
            ingest: pct(15),
            queries: pct(20),
            background: pct(10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(ByteSize::parse("64MB").unwrap().0, 64 << 20);
        assert_eq!(ByteSize::parse("512 MiB").unwrap().0, 512 << 20);
        assert_eq!(ByteSize::parse("1.5GB").unwrap().0, 3 << 29);
        assert_eq!(ByteSize::parse("100").unwrap().0, 100);
        assert!(ByteSize::parse("12XB").is_err());
        assert!(ByteSize::parse("MB").is_err());
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(512 << 20), "512 MB");
        assert_eq!(human_bytes(10), "10 B");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("30d").unwrap(), Duration::from_secs(30 * 86_400));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
        assert!(parse_duration("5 minutes").is_err());
        assert!(parse_duration("m").is_err());
        assert_eq!(format_duration(Duration::from_secs(14 * 86_400)), "2w");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
        assert_eq!(parse_retention("forever").unwrap(), None);
        assert_eq!(parse_retention("14d").unwrap(), Some(Duration::from_secs(14 * 86_400)));
    }

    #[test]
    fn spec_example_config_parses() {
        let c = Config::from_toml(
            r#"
            [server]
            bind = "0.0.0.0:8080"

            [storage]
            path = "./data"
            max_memory = "512MB"
            segment_target_size = "64MB"
            segment_max_age = "5m"
            durability = "normal"

            [retention]
            logs = "30d"
            traces = "14d"
            metrics = "30d"

            [ingest]
            max_request_size = "1MB"
            queue_capacity = 10000
            "#,
        )
        .unwrap();
        assert_eq!(c.storage.max_memory.0, 512 << 20);
        assert_eq!(c.retention.traces, Some(Duration::from_secs(14 * 86_400)));
        assert_eq!(c.ingest.queue_capacity, 10_000);
        c.validate().unwrap();
    }

    #[test]
    fn example_config_file_is_valid() {
        let c = Config::from_toml(include_str!("../../../vyrtel.example.toml")).unwrap();
        c.validate().unwrap();
    }

    /// The performance baseline's pinned configuration (docs/benchmarks.md) must stay loadable,
    /// and its explicit ingest concurrency must equal what Vyrtel would derive, so the pin
    /// documents the default rather than quietly diverging from it.
    #[test]
    fn baseline_config_file_is_valid_and_pins_the_derived_ingest_concurrency() {
        let c = Config::from_toml(include_str!("../../../dotnet/Vyrtel.Baseline/baseline.toml")).unwrap();
        c.validate().unwrap();
        assert!(!c.storage.compaction, "compaction stays off so merges cannot land mid-measurement");
        let derived =
            Config::from_toml("[storage]\nmax_memory = \"512MB\"\n[ingest]\nmax_request_size = \"1MB\"").unwrap();
        assert!(derived.ingest_admission().derived);
        assert_eq!(c.ingest.max_concurrent, derived.ingest_max_concurrent());
        assert!(c.ingest_admission().within_budget());
    }

    #[test]
    fn unknown_keys_rejected() {
        assert!(Config::from_toml("[storage]\nmax_memroy = \"1GB\"").is_err());
        assert!(Config::from_toml("[nope]\n").is_err());
    }

    #[test]
    fn env_overrides() {
        let mut c = Config::default();
        let env = |k: &str| match k {
            "VYRTEL_SERVER_BIND" => Some("127.0.0.1:9999".to_string()),
            "VYRTEL_SERVER_SHUTDOWN_TIMEOUT" => Some("1m30s".to_string()),
            "VYRTEL_STORAGE_PATH" => Some("/tmp/x".to_string()),
            "VYRTEL_STORAGE_DURABILITY" => Some("strict".to_string()),
            "VYRTEL_RETENTION_LOGS" => Some("forever".to_string()),
            "VYRTEL_AUTH_ENABLED" => Some("true".to_string()),
            _ => None,
        };
        c.apply_env(env).unwrap();
        assert_eq!(c.server.bind, "127.0.0.1:9999");
        assert_eq!(c.server.shutdown_timeout, Duration::from_secs(90));
        assert_eq!(c.storage.path, PathBuf::from("/tmp/x"));
        assert_eq!(c.storage.durability, Durability::Strict);
        assert_eq!(c.retention.logs, None);
        assert!(c.auth.enabled);
        let mut c = Config::default();
        let err = c.apply_env(|k| (k == "VYRTEL_STORAGE_DURABILITY").then(|| "maybe".to_string())).unwrap_err();
        assert!(err.contains("VYRTEL_STORAGE_DURABILITY"));
    }

    #[test]
    fn validation() {
        let mut c = Config::default();
        c.server.bind = "not an addr".into();
        assert!(c.validate().is_err());
        let mut c = Config::default();
        c.storage.max_memory = ByteSize(1 << 20);
        assert!(c.validate().is_err());
    }

    // REL-05: the drain deadline is configurable and must leave time to drain.
    #[test]
    fn shutdown_timeout() {
        assert_eq!(Config::default().server.shutdown_timeout, Duration::from_secs(8));
        let c = Config::from_toml(
            "[server]
shutdown_timeout = \"25s\"",
        )
        .unwrap();
        assert_eq!(c.server.shutdown_timeout, Duration::from_secs(25));
        c.validate().unwrap();
        let c = Config::from_toml(
            "[server]
shutdown_timeout = 0",
        )
        .unwrap();
        assert_eq!(c.validate().unwrap_err(), "server.shutdown_timeout must be positive (e.g. 8s)");
        let mut c = Config::default();
        let err = c.apply_env(|k| (k == "VYRTEL_SERVER_SHUTDOWN_TIMEOUT").then(|| "soon".to_string())).unwrap_err();
        assert!(err.contains("VYRTEL_SERVER_SHUTDOWN_TIMEOUT"), "{err}");
    }

    #[test]
    fn budgets_split_memory() {
        let b = MemoryBudgets::from_limit(512 << 20, 64 << 20);
        assert!(b.active_segments + b.index_cache + b.ingest + b.queries + b.background <= b.total);
        // 40% of 512MB = 204.8MB; logs share 50% → ~102MB → target ≤ 51MB.
        assert!(b.segment_target[0] < 64 << 20);
        assert!(b.segment_target[0] * 2 <= b.active_segments / 2 + 1);
        let big = MemoryBudgets::from_limit(8 << 30, 64 << 20);
        assert_eq!(big.segment_target[0], 64 << 20, "configured target respected when it fits");
    }

    const MB: u64 = 1 << 20;

    fn sized(memory: u64, request: u64) -> Config {
        let mut c = Config::default();
        c.storage.max_memory = ByteSize(memory);
        c.ingest.max_request_size = ByteSize(request);
        c
    }

    // TC-01: the derived limit is (15% of max_memory − fixed overhead) ÷
    // (max_request_size × factor), rounded down.
    #[test]
    fn derived_ingest_admission() {
        // Defaults: 512MB, 1MB requests. 76.8MB budget, 20MB per request.
        let c = Config::default();
        let a = c.ingest_admission();
        assert_eq!(c.ingest.max_request_size, ByteSize(MB));
        assert_eq!((a.limit, a.derived, a.budget, a.per_request), (3, true, 80_530_635, 20 * MB));
        assert_eq!(a.estimated_peak, 3 * 20 * MB + INGEST_FIXED_OVERHEAD);
        assert!(a.within_budget());
        assert_eq!(c.ingest_max_concurrent(), 3);
        c.validate().unwrap();
        // The pre-REL-14 defaults (512MB / 16MB requests) admitted 4 at a
        // time; one such request is now estimated at 320MB, so the old
        // default request size no longer fits at all.
        assert_eq!(sized(512 * MB, 16 * MB).ingest_max_concurrent(), 0);
        // Common combinations.
        for (memory, request, limit) in [
            (256 * MB, MB, 1),
            (1 << 30, MB, 7),
            (2 << 30, MB, 14),
            (2 << 30, 4 * MB, 3),
            (8 << 30, 16 * MB, 3),
            (128 * MB, 256 << 10, 1),
            (256 * MB, 256 << 10, 5),
        ] {
            assert_eq!(sized(memory, request).ingest_max_concurrent(), limit, "{memory} / {request}");
        }
        // Rounds down: exactly n requests plus the overhead fit at the
        // boundary, one byte more per request fits n − 1.
        let mut c = Config::default();
        let budget = c.budgets().ingest;
        for n in [1u64, 2, 3] {
            c.ingest.max_request_size = ByteSize((budget - INGEST_FIXED_OVERHEAD) / n / INGEST_BYTES_PER_REQUEST_BYTE);
            assert_eq!(c.ingest_max_concurrent(), n as usize);
            c.ingest.max_request_size.0 += 1;
            assert_eq!(c.ingest_max_concurrent(), n as usize - 1);
        }
        // Upper cap.
        let c = sized(64 << 30, 1024);
        assert_eq!(c.ingest_max_concurrent(), INGEST_MAX_DERIVED_CONCURRENCY);
        c.validate().unwrap();
    }

    // TC-01 / AC2: one maximum-size request plus overhead must fit, or the
    // configuration is rejected with what to change. No minimum of two.
    #[test]
    fn derived_ingest_admission_that_cannot_fit_one_request_is_invalid() {
        // 219MB is the smallest max_memory that fits one 1MB request.
        let ok = sized(219 * MB, MB);
        assert_eq!(ok.ingest_max_concurrent(), 1);
        ok.validate().unwrap();
        let low = sized(218 * MB, MB);
        assert_eq!(low.ingest_max_concurrent(), 0);
        let err = low.validate().unwrap_err();
        assert!(err.contains("ingest budget too small"), "{err}");
        assert!(err.contains("one 1 MB request needs an estimated 32.8 MB"), "{err}");
        assert!(err.contains("increase storage.max_memory to at least 219MB"), "{err}");
        assert!(err.contains("reduce ingest.max_request_size to at most 1021KB"), "{err}");
        assert!(err.contains("set ingest.max_concurrent explicitly"), "{err}");
        assert!(sized(218 * MB, 1021 << 10).validate().is_ok());
        assert!(sized(218 * MB, 1022 << 10).validate().is_err());

        // At the 64MB minimum the ingest share (9.6MB) is below the fixed
        // overhead alone: no request size fits, so none is suggested.
        let err = sized(64 * MB, MB).validate().unwrap_err();
        assert!(err.contains("at least 219MB"), "{err}");
        assert!(!err.contains("reduce ingest.max_request_size"), "{err}");
        assert_eq!(sized(64 * MB, 1024).ingest_max_concurrent(), 0);

        // An explicit limit makes it valid again (with a warning).
        let mut c = sized(64 * MB, MB);
        c.ingest.max_concurrent = 1;
        c.validate().unwrap();
        assert!(c.ingest_admission().warning().is_some());
    }

    // TC-01: extreme and invalid values neither wrap nor panic.
    #[test]
    fn ingest_admission_arithmetic_saturates() {
        for request in [0, 1, u64::MAX / INGEST_BYTES_PER_REQUEST_BYTE + 1, u64::MAX] {
            for memory in [0, 64 * MB, u64::MAX] {
                let a = sized(memory, request).ingest_admission();
                assert!(a.limit <= INGEST_MAX_DERIVED_CONCURRENCY);
                assert!(a.per_request >= 1);
            }
        }
        let huge = sized(u64::MAX, u64::MAX).ingest_admission();
        assert_eq!((huge.limit, huge.per_request), (0, u64::MAX));
        assert!(huge.invalid_message().contains("ingest budget too small"));
        // Zero is caught by the 1KB minimum, not by a division by zero.
        assert!(sized(512 * MB, 0).validate().unwrap_err().contains("at least 1KB"));

        let mut c = Config::default();
        c.ingest.max_concurrent = INGEST_MAX_CONCURRENT_LIMIT;
        c.validate().unwrap();
        let a = c.ingest_admission();
        assert_eq!(a.limit, INGEST_MAX_CONCURRENT_LIMIT);
        assert!(a.estimated_peak > a.budget);
        c.ingest.max_request_size = ByteSize(u64::MAX);
        assert_eq!(c.ingest_admission().estimated_peak, u64::MAX);
        c.ingest.max_concurrent = usize::MAX;
        let err = c.validate().unwrap_err();
        assert!(err.contains("ingest.max_concurrent must be at most"), "{err}");
        assert_eq!(c.ingest_admission().estimated_peak, u64::MAX);
    }

    // TC-02 / AC4: explicit values are honoured as before, inside or
    // outside the derived range; only over-budget ones warn.
    #[test]
    fn explicit_ingest_concurrency_is_honoured_and_warns_over_budget() {
        let mut c = Config::default();
        assert_eq!(c.ingest_admission().warning(), None, "derived never warns");
        c.ingest.max_concurrent = 1;
        assert_eq!(c.ingest_max_concurrent(), 1);
        assert_eq!(c.ingest_admission().warning(), None);
        // 3 × 20MB + 12.75MB = 72.75MB fits in 76.8MB; 4 needs 92.75MB.
        c.ingest.max_concurrent = 3;
        assert!(!c.ingest_admission().derived);
        assert_eq!(c.ingest_admission().warning(), None);
        c.ingest.max_concurrent = 4;
        assert_eq!(c.ingest_max_concurrent(), 4);
        let w = c.ingest_admission().warning().expect("over budget");
        for part in [
            "ingest.max_concurrent = 4",
            "ingest.max_request_size = 1 MB",
            "estimated 92.8 MB",
            "76.8 MB ingest budget",
            "the limit is honoured",
        ] {
            assert!(w.contains(part), "{part:?} in {w}");
        }
        // Above the derived cap, and with the pre-REL-14 16MB requests.
        c.ingest.max_concurrent = 5000;
        assert_eq!(c.ingest_max_concurrent(), 5000);
        c.validate().unwrap();
        let mut old = sized(512 * MB, 16 * MB);
        old.ingest.max_concurrent = 4;
        old.validate().unwrap();
        assert_eq!(old.ingest_max_concurrent(), 4);
        assert!(old.ingest_admission().warning().unwrap().contains("estimated 1.3 GB"));
    }

    // TC-02: TOML and environment precedence for both settings.
    #[test]
    fn ingest_admission_config_sources() {
        let mut c = Config::from_toml(
            r#"
            [storage]
            max_memory = "1GB"
            [ingest]
            max_concurrent = 3
            max_request_size = "2MB"
            "#,
        )
        .unwrap();
        assert_eq!(c.ingest_max_concurrent(), 3);
        assert!(!c.ingest_admission().derived);
        // The environment wins over the file, including back to derived.
        c.apply_env(|k| (k == "VYRTEL_INGEST_MAX_CONCURRENT").then(|| "0".to_string())).unwrap();
        assert!(c.ingest_admission().derived);
        assert_eq!(c.ingest_max_concurrent(), 3, "(153.6MB − 12.75MB) ÷ 40MB");
        c.apply_env(|k| (k == "VYRTEL_INGEST_MAX_REQUEST_SIZE").then(|| "512KB".to_string())).unwrap();
        assert_eq!(c.ingest_max_concurrent(), 14);
        c.apply_env(|k| (k == "VYRTEL_INGEST_MAX_CONCURRENT").then(|| "7".to_string())).unwrap();
        assert_eq!(c.ingest_max_concurrent(), 7);
        assert!(c.apply_env(|k| (k == "VYRTEL_INGEST_MAX_CONCURRENT").then(|| "lots".to_string())).is_err());
        assert!(c.apply_env(|k| (k == "VYRTEL_INGEST_MAX_CONCURRENT").then(|| "-1".to_string())).is_err());
        // A file that is invalid only because of derived sizing.
        let c = Config::from_toml("[storage]\nmax_memory = \"64MB\"").unwrap();
        assert!(c.validate().unwrap_err().contains("ingest budget too small"));
    }
}
