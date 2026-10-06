//! Configuration: defaults ← TOML file ← `OBSERVER_*` environment ← CLI.
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { bind: "0.0.0.0:8080".into() }
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
}

impl Default for IngestSection {
    fn default() -> Self {
        Self { max_request_size: ByteSize(16 << 20), queue_capacity: 10_000, ack_timeout: Duration::from_secs(30) }
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
    /// exposing Observer beyond a trusted network.
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
    "OBSERVER_SERVER_BIND",
    "OBSERVER_STORAGE_PATH",
    "OBSERVER_STORAGE_MAX_MEMORY",
    "OBSERVER_STORAGE_SEGMENT_TARGET_SIZE",
    "OBSERVER_STORAGE_SEGMENT_MAX_AGE",
    "OBSERVER_STORAGE_DURABILITY",
    "OBSERVER_STORAGE_FSYNC_INTERVAL",
    "OBSERVER_STORAGE_COMPACTION",
    "OBSERVER_RETENTION_LOGS",
    "OBSERVER_RETENTION_TRACES",
    "OBSERVER_RETENTION_METRICS",
    "OBSERVER_INGEST_MAX_REQUEST_SIZE",
    "OBSERVER_INGEST_QUEUE_CAPACITY",
    "OBSERVER_QUERY_TIMEOUT",
    "OBSERVER_QUERY_MAX_CONCURRENT",
    "OBSERVER_AUTH_ENABLED",
    "OBSERVER_AUTH_ADMIN_USERNAME",
    "OBSERVER_AUTH_ADMIN_PASSWORD",
    "OBSERVER_ALERTS_ENABLED",
    "OBSERVER_LOG_LEVEL",
    "OBSERVER_LOG_FORMAT",
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

    /// Apply `OBSERVER_<SECTION>_<KEY>` overrides.
    pub fn apply_env(&mut self, get: impl Fn(&str) -> Option<String>) -> Result<(), String> {
        let wrap = |k: &str, r: Result<(), String>| r.map_err(|e| format!("{k}: {e}"));
        for key in ENV_VARS {
            let Some(v) = get(key) else { continue };
            let r: Result<(), String> = (|| {
                match *key {
                    "OBSERVER_SERVER_BIND" => self.server.bind = v.clone(),
                    "OBSERVER_STORAGE_PATH" => self.storage.path = PathBuf::from(&v),
                    "OBSERVER_STORAGE_MAX_MEMORY" => self.storage.max_memory = ByteSize::parse(&v)?,
                    "OBSERVER_STORAGE_SEGMENT_TARGET_SIZE" => self.storage.segment_target_size = ByteSize::parse(&v)?,
                    "OBSERVER_STORAGE_SEGMENT_MAX_AGE" => self.storage.segment_max_age = parse_duration(&v)?,
                    "OBSERVER_STORAGE_DURABILITY" => {
                        self.storage.durability = match v.trim().to_ascii_lowercase().as_str() {
                            "normal" => Durability::Normal,
                            "strict" => Durability::Strict,
                            _ => return Err("must be 'normal' or 'strict'".into()),
                        }
                    }
                    "OBSERVER_STORAGE_FSYNC_INTERVAL" => self.storage.fsync_interval = parse_duration(&v)?,
                    "OBSERVER_STORAGE_COMPACTION" => self.storage.compaction = parse_bool(&v)?,
                    "OBSERVER_RETENTION_LOGS" => self.retention.logs = parse_retention(&v)?,
                    "OBSERVER_RETENTION_TRACES" => self.retention.traces = parse_retention(&v)?,
                    "OBSERVER_RETENTION_METRICS" => self.retention.metrics = parse_retention(&v)?,
                    "OBSERVER_INGEST_MAX_REQUEST_SIZE" => self.ingest.max_request_size = ByteSize::parse(&v)?,
                    "OBSERVER_INGEST_QUEUE_CAPACITY" => {
                        self.ingest.queue_capacity = v.trim().parse().map_err(|_| "must be a number".to_string())?
                    }
                    "OBSERVER_QUERY_TIMEOUT" => self.query.timeout = parse_duration(&v)?,
                    "OBSERVER_QUERY_MAX_CONCURRENT" => {
                        self.query.max_concurrent = v.trim().parse().map_err(|_| "must be a number".to_string())?
                    }
                    "OBSERVER_AUTH_ENABLED" => self.auth.enabled = parse_bool(&v)?,
                    "OBSERVER_AUTH_ADMIN_USERNAME" => self.auth.admin_username = v.clone(),
                    "OBSERVER_AUTH_ADMIN_PASSWORD" => self.auth.admin_password = Some(v.clone()),
                    "OBSERVER_ALERTS_ENABLED" => self.alerts.enabled = parse_bool(&v)?,
                    "OBSERVER_LOG_LEVEL" => self.log.level = v.clone(),
                    "OBSERVER_LOG_FORMAT" => {
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
        if self.query.max_concurrent == 0 {
            return Err("query.max_concurrent must be positive".into());
        }
        if !(1..=22).contains(&self.storage.zstd_level) {
            return Err("storage.zstd_level must be between 1 and 22".into());
        }
        if self.storage.block_events == 0 {
            return Err("storage.block_events must be positive".into());
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
            max_request_size = "16MB"
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
        let c = Config::from_toml(include_str!("../../../observer.example.toml")).unwrap();
        c.validate().unwrap();
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
            "OBSERVER_SERVER_BIND" => Some("127.0.0.1:9999".to_string()),
            "OBSERVER_STORAGE_PATH" => Some("/tmp/x".to_string()),
            "OBSERVER_STORAGE_DURABILITY" => Some("strict".to_string()),
            "OBSERVER_RETENTION_LOGS" => Some("forever".to_string()),
            "OBSERVER_AUTH_ENABLED" => Some("true".to_string()),
            _ => None,
        };
        c.apply_env(env).unwrap();
        assert_eq!(c.server.bind, "127.0.0.1:9999");
        assert_eq!(c.storage.path, PathBuf::from("/tmp/x"));
        assert_eq!(c.storage.durability, Durability::Strict);
        assert_eq!(c.retention.logs, None);
        assert!(c.auth.enabled);
        let mut c = Config::default();
        let err = c.apply_env(|k| (k == "OBSERVER_STORAGE_DURABILITY").then(|| "maybe".to_string())).unwrap_err();
        assert!(err.contains("OBSERVER_STORAGE_DURABILITY"));
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
}
