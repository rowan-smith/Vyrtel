//! The common telemetry envelope.
//!
//! This is the *domain* model. It is deliberately not the on-disk format:
//! storage owns its own versioned binary encoding, so these structs can evolve
//! without breaking existing data directories.

use std::fmt;

use crate::time::Timestamp;
use crate::value::Fields;

/// Sequence number assigned by the storage writer. Unique and monotonic
/// within a signal stream; `(timestamp, id)` gives a total order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct EventId(pub u64);

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl EventId {
    pub fn parse(s: &str) -> Option<Self> {
        u64::from_str_radix(s, 16).ok().map(EventId)
    }
}

/// Trace identifier. OTLP trace IDs are 16 bytes rendered as lowercase hex,
/// but native producers send whatever they like ("abc123"), so we keep a
/// string and only normalise case when it is hexadecimal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraceId(String);

/// Span identifier, normalised like [`TraceId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId(String);

macro_rules! id_type {
    ($t:ident) => {
        impl $t {
            /// Returns `None` for empty / all-zero identifiers (OTLP uses
            /// zeroed bytes to mean "absent").
            pub fn new(s: &str) -> Option<Self> {
                let s = s.trim();
                if s.is_empty() || s.bytes().all(|b| b == b'0') {
                    return None;
                }
                Some($t(normalize_id(s)))
            }

            pub fn from_bytes(b: &[u8]) -> Option<Self> {
                if b.is_empty() || b.iter().all(|x| *x == 0) {
                    return None;
                }
                let mut s = String::with_capacity(b.len() * 2);
                for byte in b {
                    s.push_str(&format!("{byte:02x}"));
                }
                Some($t(s))
            }

            /// Construct without normalisation; used by decoders that read
            /// already-normalised values back from disk.
            pub fn from_stored(s: String) -> Self {
                $t(s)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(TraceId);
id_type!(SpanId);

/// Lowercase hexadecimal identifiers so that `ABC` and `abc` match; leave
/// anything else untouched.
pub fn normalize_id(s: &str) -> String {
    if s.bytes().all(|b| b.is_ascii_hexdigit()) { s.to_ascii_lowercase() } else { s.to_string() }
}

/// Which stream an event belongs to. Each signal is stored separately so
/// retention can be applied per signal at segment granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Signal {
    Logs,
    Traces,
    Metrics,
}

impl Signal {
    pub const ALL: [Signal; 3] = [Signal::Logs, Signal::Traces, Signal::Metrics];

    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Logs => "logs",
            Signal::Traces => "traces",
            Signal::Metrics => "metrics",
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Signal::Logs => 1,
            Signal::Traces => 2,
            Signal::Metrics => 3,
        }
    }

    pub fn from_code(c: u8) -> Option<Self> {
        match c {
            1 => Some(Signal::Logs),
            2 => Some(Signal::Traces),
            3 => Some(Signal::Metrics),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace,
    Debug,
    Information,
    Warning,
    Error,
    Fatal,
}

impl Level {
    pub const ALL: [Level; 6] =
        [Level::Trace, Level::Debug, Level::Information, Level::Warning, Level::Error, Level::Fatal];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "Trace",
            Level::Debug => "Debug",
            Level::Information => "Information",
            Level::Warning => "Warning",
            Level::Error => "Error",
            Level::Fatal => "Fatal",
        }
    }

    /// Stable numeric code used by storage and bitmap indexes (1..=6).
    pub fn code(self) -> u8 {
        match self {
            Level::Trace => 1,
            Level::Debug => 2,
            Level::Information => 3,
            Level::Warning => 4,
            Level::Error => 5,
            Level::Fatal => 6,
        }
    }

    pub fn from_code(c: u8) -> Option<Self> {
        Level::ALL.get((c as usize).checked_sub(1)?).copied()
    }

    /// Accept the many spellings producers use (Serilog, log4j, syslog, OTel).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" | "verbose" | "vrb" | "trc" | "finest" | "finer" => Some(Level::Trace),
            "debug" | "dbg" | "fine" => Some(Level::Debug),
            "information" | "info" | "informational" | "inf" | "notice" => Some(Level::Information),
            "warning" | "warn" | "wrn" => Some(Level::Warning),
            "error" | "err" | "eror" | "severe" => Some(Level::Error),
            "fatal" | "critical" | "crit" | "ftl" | "panic" | "emerg" | "emergency" | "alert" => Some(Level::Fatal),
            _ => None,
        }
    }

    /// Map an OTLP `SeverityNumber` (1..=24) onto a level.
    pub fn from_otlp_severity(n: i32) -> Option<Self> {
        match n {
            1..=4 => Some(Level::Trace),
            5..=8 => Some(Level::Debug),
            9..=12 => Some(Level::Information),
            13..=16 => Some(Level::Warning),
            17..=20 => Some(Level::Error),
            21..=24 => Some(Level::Fatal),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Exception {
    pub kind: Option<String>,
    pub message: Option<String>,
    pub stack_trace: Option<String>,
}

impl Exception {
    pub fn is_empty(&self) -> bool {
        self.kind.is_none() && self.message.is_none() && self.stack_trace.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogEvent {
    pub level: Level,
    pub message: String,
    pub message_template: Option<String>,
    pub exception: Option<Exception>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanKind {
    #[default]
    Unspecified,
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl SpanKind {
    pub fn code(self) -> u8 {
        match self {
            SpanKind::Unspecified => 0,
            SpanKind::Internal => 1,
            SpanKind::Server => 2,
            SpanKind::Client => 3,
            SpanKind::Producer => 4,
            SpanKind::Consumer => 5,
        }
    }

    pub fn from_code(c: u8) -> Self {
        match c {
            1 => SpanKind::Internal,
            2 => SpanKind::Server,
            3 => SpanKind::Client,
            4 => SpanKind::Producer,
            5 => SpanKind::Consumer,
            _ => SpanKind::Unspecified,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SpanKind::Unspecified => "Unspecified",
            SpanKind::Internal => "Internal",
            SpanKind::Server => "Server",
            SpanKind::Client => "Client",
            SpanKind::Producer => "Producer",
            SpanKind::Consumer => "Consumer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusCode {
    #[default]
    Unset,
    Ok,
    Error,
}

impl StatusCode {
    pub fn code(self) -> u8 {
        match self {
            StatusCode::Unset => 0,
            StatusCode::Ok => 1,
            StatusCode::Error => 2,
        }
    }

    pub fn from_code(c: u8) -> Self {
        match c {
            1 => StatusCode::Ok,
            2 => StatusCode::Error,
            _ => StatusCode::Unset,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StatusCode::Unset => "Unset",
            StatusCode::Ok => "Ok",
            StatusCode::Error => "Error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpanStatus {
    pub code: StatusCode,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpanEventItem {
    pub timestamp: Timestamp,
    pub name: String,
    pub attributes: Fields,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpanLink {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub attributes: Fields,
}

/// A span. Its start time is the envelope `timestamp`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanEvent {
    pub parent_span_id: Option<SpanId>,
    pub name: String,
    pub kind: SpanKind,
    pub duration_nanos: u64,
    pub status: SpanStatus,
    pub events: Vec<SpanEventItem>,
    pub links: Vec<SpanLink>,
}

impl SpanEvent {
    pub fn duration_ms(&self) -> f64 {
        self.duration_nanos as f64 / 1e6
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Gauge,
    Sum,
    Histogram,
}

impl MetricKind {
    pub fn code(self) -> u8 {
        match self {
            MetricKind::Gauge => 1,
            MetricKind::Sum => 2,
            MetricKind::Histogram => 3,
        }
    }

    pub fn from_code(c: u8) -> Option<Self> {
        match c {
            1 => Some(MetricKind::Gauge),
            2 => Some(MetricKind::Sum),
            3 => Some(MetricKind::Histogram),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MetricKind::Gauge => "gauge",
            MetricKind::Sum => "sum",
            MetricKind::Histogram => "histogram",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Temporality {
    #[default]
    Unspecified,
    Delta,
    Cumulative,
}

impl Temporality {
    pub fn code(self) -> u8 {
        match self {
            Temporality::Unspecified => 0,
            Temporality::Delta => 1,
            Temporality::Cumulative => 2,
        }
    }

    pub fn from_code(c: u8) -> Self {
        match c {
            1 => Temporality::Delta,
            2 => Temporality::Cumulative,
            _ => Temporality::Unspecified,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Temporality::Unspecified => "unspecified",
            Temporality::Delta => "delta",
            Temporality::Cumulative => "cumulative",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Histogram {
    pub count: u64,
    pub sum: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub bucket_counts: Vec<u64>,
    pub explicit_bounds: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetricValue {
    Number(f64),
    Histogram(Histogram),
}

/// One metric data point.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricEvent {
    pub name: String,
    pub description: Option<String>,
    pub unit: Option<String>,
    pub kind: MetricKind,
    pub temporality: Temporality,
    pub monotonic: bool,
    pub value: MetricValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryPayload {
    Log(LogEvent),
    Span(SpanEvent),
    Metric(MetricEvent),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryEvent {
    pub id: EventId,
    pub timestamp: Timestamp,
    pub observed_timestamp: Timestamp,

    pub service: Option<String>,
    pub environment: Option<String>,

    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,

    pub resource: Fields,
    pub attributes: Fields,

    pub payload: TelemetryPayload,
}

impl TelemetryEvent {
    pub fn signal(&self) -> Signal {
        match self.payload {
            TelemetryPayload::Log(_) => Signal::Logs,
            TelemetryPayload::Span(_) => Signal::Traces,
            TelemetryPayload::Metric(_) => Signal::Metrics,
        }
    }

    pub fn level(&self) -> Option<Level> {
        match &self.payload {
            TelemetryPayload::Log(l) => Some(l.level),
            _ => None,
        }
    }

    /// The primary text of an event: log message, span name or metric name.
    pub fn message(&self) -> &str {
        match &self.payload {
            TelemetryPayload::Log(l) => &l.message,
            TelemetryPayload::Span(s) => &s.name,
            TelemetryPayload::Metric(m) => &m.name,
        }
    }

    pub fn as_log(&self) -> Option<&LogEvent> {
        match &self.payload {
            TelemetryPayload::Log(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_span(&self) -> Option<&SpanEvent> {
        match &self.payload {
            TelemetryPayload::Span(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_metric(&self) -> Option<&MetricEvent> {
        match &self.payload {
            TelemetryPayload::Metric(m) => Some(m),
            _ => None,
        }
    }

    /// Approximate in-memory footprint. Used to bound the active segment.
    pub fn approx_size(&self) -> usize {
        let opt = |s: &Option<String>| s.as_ref().map_or(0, |s| s.len());
        let base = std::mem::size_of::<TelemetryEvent>()
            + opt(&self.service)
            + opt(&self.environment)
            + self.trace_id.as_ref().map_or(0, |t| t.as_str().len())
            + self.span_id.as_ref().map_or(0, |t| t.as_str().len())
            + self.resource.approx_size()
            + self.attributes.approx_size();
        base + match &self.payload {
            TelemetryPayload::Log(l) => {
                l.message.len()
                    + opt(&l.message_template)
                    + l.exception.as_ref().map_or(0, |e| opt(&e.kind) + opt(&e.message) + opt(&e.stack_trace))
            }
            TelemetryPayload::Span(s) => {
                s.name.len()
                    + opt(&s.status.message)
                    + s.events.iter().map(|e| 48 + e.name.len() + e.attributes.approx_size()).sum::<usize>()
                    + s.links.iter().map(|l| 64 + l.attributes.approx_size()).sum::<usize>()
            }
            TelemetryPayload::Metric(m) => {
                m.name.len()
                    + opt(&m.description)
                    + opt(&m.unit)
                    + match &m.value {
                        MetricValue::Number(_) => 0,
                        MetricValue::Histogram(h) => h.bucket_counts.len() * 8 + h.explicit_bounds.len() * 8,
                    }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parsing_and_codes() {
        assert_eq!(Level::parse("ERR"), Some(Level::Error));
        assert_eq!(Level::parse(" warn "), Some(Level::Warning));
        assert_eq!(Level::parse("Information"), Some(Level::Information));
        assert_eq!(Level::parse("critical"), Some(Level::Fatal));
        assert_eq!(Level::parse("bogus"), None);
        for l in Level::ALL {
            assert_eq!(Level::from_code(l.code()), Some(l));
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
        assert_eq!(Level::from_code(0), None);
        assert!(Level::Error > Level::Warning);
    }

    #[test]
    fn otlp_severity_mapping() {
        assert_eq!(Level::from_otlp_severity(0), None);
        assert_eq!(Level::from_otlp_severity(1), Some(Level::Trace));
        assert_eq!(Level::from_otlp_severity(9), Some(Level::Information));
        assert_eq!(Level::from_otlp_severity(17), Some(Level::Error));
        assert_eq!(Level::from_otlp_severity(24), Some(Level::Fatal));
        assert_eq!(Level::from_otlp_severity(25), None);
    }

    #[test]
    fn ids_normalise() {
        assert_eq!(TraceId::new("ABC123").unwrap().as_str(), "abc123");
        assert_eq!(TraceId::new("Not-Hex").unwrap().as_str(), "Not-Hex");
        assert!(TraceId::new("0000").is_none());
        assert!(TraceId::new("  ").is_none());
        assert_eq!(SpanId::from_bytes(&[0xde, 0xad, 0xbe, 0xef]).unwrap().as_str(), "deadbeef");
        assert!(SpanId::from_bytes(&[0, 0]).is_none());
    }

    #[test]
    fn event_id_hex() {
        let id = EventId(42);
        assert_eq!(id.to_string(), "000000000000002a");
        assert_eq!(EventId::parse(&id.to_string()), Some(id));
    }
}
