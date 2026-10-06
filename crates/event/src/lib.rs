use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Log,
    Span,
    Metric,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::Log => "log",
            EventType::Span => "span",
            EventType::Metric => "metric",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "log" => Some(EventType::Log),
            "span" => Some(EventType::Span),
            "metric" => Some(EventType::Metric),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Information,
    Warning,
    Error,
    Fatal,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Information => "information",
            LogLevel::Warning => "warning",
            LogLevel::Error => "error",
            LogLevel::Fatal => "fatal",
        }
    }

    pub fn short(&self) -> &'static str {
        match self {
            LogLevel::Trace => "TRC",
            LogLevel::Debug => "DBG",
            LogLevel::Information => "INF",
            LogLevel::Warning => "WRN",
            LogLevel::Error => "ERR",
            LogLevel::Fatal => "FTL",
        }
    }

    /// Normalise common severity strings into canonical log levels.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" | "verbose" | "vrb" | "trc" => Some(LogLevel::Trace),
            "debug" | "dbg" => Some(LogLevel::Debug),
            "information" | "info" | "informational" | "inf" => Some(LogLevel::Information),
            "warning" | "warn" | "wrn" => Some(LogLevel::Warning),
            "error" | "err" => Some(LogLevel::Error),
            "fatal" | "critical" | "crit" | "ftl" | "panic" => Some(LogLevel::Fatal),
            _ => None,
        }
    }

    pub fn from_otlp_severity_number(n: i32) -> Self {
        match n {
            1..=4 => LogLevel::Trace,
            5..=8 => LogLevel::Debug,
            9..=12 => LogLevel::Information,
            13..=16 => LogLevel::Warning,
            17..=20 => LogLevel::Error,
            21..=24 => LogLevel::Fatal,
            _ if n <= 0 => LogLevel::Information,
            _ => LogLevel::Fatal,
        }
    }

    pub fn rank(&self) -> u8 {
        match self {
            LogLevel::Trace => 0,
            LogLevel::Debug => 1,
            LogLevel::Information => 2,
            LogLevel::Warning => 3,
            LogLevel::Error => 4,
            LogLevel::Fatal => 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub event_type: EventType,
    pub level: Option<LogLevel>,
    pub message: Option<String>,
    pub message_template: Option<String>,
    pub service: Option<String>,
    pub environment: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub duration_ns: Option<u64>,
    pub stacktrace: Option<String>,
    pub attributes: Map<String, Value>,
}

impl Event {
    pub fn new_log() -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            event_type: EventType::Log,
            level: Some(LogLevel::Information),
            message: None,
            message_template: None,
            service: None,
            environment: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
            duration_ns: None,
            stacktrace: None,
            attributes: Map::new(),
        }
    }

    pub fn new_span() -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            event_type: EventType::Span,
            level: None,
            message: None,
            message_template: None,
            service: None,
            environment: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
            duration_ns: None,
            stacktrace: None,
            attributes: Map::new(),
        }
    }
}

/// Incoming JSON payload for log/event ingestion (id is always server-generated).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestEventRequest {
    pub timestamp: Option<DateTime<Utc>>,
    pub event_type: Option<EventType>,
    pub level: Option<String>,
    pub message: Option<String>,
    pub message_template: Option<String>,
    pub service: Option<String>,
    pub environment: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub duration_ns: Option<u64>,
    #[serde(default, alias = "exception")]
    pub stacktrace: Option<String>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
}

impl IngestEventRequest {
    pub fn into_event(self) -> Event {
        let level = self.level.as_deref().and_then(LogLevel::parse);
        let event_type = self.event_type.unwrap_or(EventType::Log);
        let mut attributes = self.attributes;
        // Prefer exception.stacktrace (and aliases) when present; also strip those keys
        // from attributes so the UI can render a formatted stacktrace panel instead.
        let from_attrs = take_stacktrace_from_attributes(&mut attributes);
        let stacktrace = from_attrs.or(self.stacktrace);
        strip_stacktrace_attributes(&mut attributes);
        Event {
            id: Uuid::new_v4(),
            timestamp: self.timestamp.unwrap_or_else(Utc::now),
            event_type,
            level,
            message: self.message,
            message_template: self.message_template,
            service: self.service,
            environment: self.environment,
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_span_id: self.parent_span_id,
            duration_ns: self.duration_ns,
            stacktrace,
            attributes,
        }
    }
}

fn take_stacktrace_from_attributes(attributes: &mut Map<String, Value>) -> Option<String> {
    const KEYS: &[&str] = &[
        "exception.stacktrace",
        "exception.stackTrace",
        "exception.stack_trace",
        "stacktrace",
        "stackTrace",
        "stack_trace",
        "error.stack",
        "error.stacktrace",
        "error.stack_trace",
        "exception",
        "@x",
    ];
    for key in KEYS {
        if let Some(value) = attributes.remove(*key) {
            match value {
                Value::String(s) if !s.is_empty() => return Some(s),
                other => {
                    // Keep non-string values as attributes.
                    attributes.insert((*key).to_string(), other);
                }
            }
        }
    }
    None
}

fn strip_stacktrace_attributes(attributes: &mut Map<String, Value>) {
    const KEYS: &[&str] = &[
        "exception.stacktrace",
        "exception.stackTrace",
        "exception.stack_trace",
        "stacktrace",
        "stackTrace",
        "stack_trace",
        "error.stack",
        "error.stacktrace",
        "error.stack_trace",
        "@x",
    ];
    for key in KEYS {
        attributes.remove(*key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_level_aliases() {
        assert_eq!(LogLevel::parse("ERR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("warn"), Some(LogLevel::Warning));
        assert_eq!(LogLevel::parse("info"), Some(LogLevel::Information));
        assert_eq!(LogLevel::parse("critical"), Some(LogLevel::Fatal));
    }

    #[test]
    fn ingest_assigns_server_id_and_timestamp() {
        let req = IngestEventRequest {
            timestamp: None,
            event_type: None,
            level: Some("error".into()),
            message: Some("Payment failed".into()),
            message_template: None,
            service: Some("billing".into()),
            environment: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
            duration_ns: None,
            stacktrace: None,
            attributes: Map::new(),
        };
        let event = req.into_event();
        assert_eq!(event.level, Some(LogLevel::Error));
        assert_eq!(event.service.as_deref(), Some("billing"));
        assert_eq!(event.event_type, EventType::Log);
    }

    #[test]
    fn lifts_stacktrace_from_attributes() {
        let mut attrs = Map::new();
        attrs.insert(
            "exception.stacktrace".into(),
            Value::String("at billing.pay()\nat api.handle()".into()),
        );
        let req = IngestEventRequest {
            timestamp: None,
            event_type: None,
            level: Some("error".into()),
            message: Some("boom".into()),
            message_template: None,
            service: None,
            environment: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
            duration_ns: None,
            stacktrace: None,
            attributes: attrs,
        };
        let event = req.into_event();
        assert_eq!(
            event.stacktrace.as_deref(),
            Some("at billing.pay()\nat api.handle()")
        );
        assert!(!event.attributes.contains_key("exception.stacktrace"));
    }

    #[test]
    fn lifts_error_stack_and_aliases() {
        for key in ["exception.stack_trace", "error.stack", "error.stacktrace", "@x"] {
            let mut attrs = Map::new();
            attrs.insert(key.into(), Value::String("trace text".into()));
            let req = IngestEventRequest {
                timestamp: None,
                event_type: None,
                level: Some("error".into()),
                message: Some("boom".into()),
                message_template: None,
                service: None,
                environment: None,
                trace_id: None,
                span_id: None,
                parent_span_id: None,
                duration_ns: None,
                stacktrace: None,
                attributes: attrs,
            };
            let event = req.into_event();
            assert_eq!(event.stacktrace.as_deref(), Some("trace text"));
            assert!(!event.attributes.contains_key(key));
        }
    }
}
