//! Field-name resolution.
//!
//! Well-known names (case-insensitive) map to envelope fields; everything
//! else is a structured property, looked up in event attributes first and
//! then resource attributes. Explicit prefixes pick one namespace:
//! `attributes.x` / `properties.x` / `resource.x`.

use storage::segment::{Column, ColumnSet, paths};
use telemetry::Signal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldRef {
    Timestamp,
    Level,
    Service,
    Environment,
    /// Log message, span name or metric name.
    Message,
    TraceId,
    SpanId,
    MessageTemplate,
    ExceptionType,
    ExceptionMessage,
    StackTrace,
    ParentSpanId,
    DurationMs,
    Status,
    SpanKind,
    MetricValue,
    Unit,
    MetricKind,
    /// Attribute, falling back to resource attribute.
    Attr(String),
    AttrOnly(String),
    Resource(String),
}

pub fn resolve(field: &str, signal: Signal) -> FieldRef {
    if let Some(rest) = field.strip_prefix("attributes.").or_else(|| field.strip_prefix("properties.")) {
        return FieldRef::AttrOnly(rest.to_string());
    }
    if let Some(rest) = field.strip_prefix("resource.") {
        return FieldRef::Resource(rest.to_string());
    }
    let lower = field.to_ascii_lowercase();
    let common = match lower.as_str() {
        "timestamp" | "@t" | "time" => Some(FieldRef::Timestamp),
        "service" | "service.name" => Some(FieldRef::Service),
        "environment" | "env" | "deployment.environment" => Some(FieldRef::Environment),
        "message" | "@m" | "msg" => Some(FieldRef::Message),
        "traceid" | "trace_id" | "@tr" => Some(FieldRef::TraceId),
        "spanid" | "span_id" | "@sp" => Some(FieldRef::SpanId),
        _ => None,
    };
    if let Some(f) = common {
        return f;
    }
    let specific = match signal {
        Signal::Logs => match lower.as_str() {
            "level" | "@l" | "severity" => Some(FieldRef::Level),
            "messagetemplate" | "@mt" | "template" => Some(FieldRef::MessageTemplate),
            "exception.type" => Some(FieldRef::ExceptionType),
            "exception.message" => Some(FieldRef::ExceptionMessage),
            "exception.stacktrace" | "stacktrace" | "exception" | "@x" => Some(FieldRef::StackTrace),
            _ => None,
        },
        Signal::Traces => match lower.as_str() {
            "name" | "operation" => Some(FieldRef::Message),
            "parentspanid" | "parent_span_id" => Some(FieldRef::ParentSpanId),
            "durationms" | "duration" => Some(FieldRef::DurationMs),
            "status" => Some(FieldRef::Status),
            "spankind" | "kind" => Some(FieldRef::SpanKind),
            _ => None,
        },
        Signal::Metrics => match lower.as_str() {
            "name" | "metric" => Some(FieldRef::Message),
            "value" => Some(FieldRef::MetricValue),
            "unit" => Some(FieldRef::Unit),
            "metrickind" | "type" => Some(FieldRef::MetricKind),
            _ => None,
        },
    };
    specific.unwrap_or_else(|| FieldRef::Attr(field.to_string()))
}

impl FieldRef {
    /// Segment columns needed to evaluate this field. Anything stored in the
    /// event body requires the full event.
    pub fn columns(&self) -> ColumnSet {
        let base = ColumnSet::new();
        match self {
            FieldRef::Timestamp => base,
            FieldRef::Level | FieldRef::Service | FieldRef::Environment => base.with(Column::Meta),
            FieldRef::TraceId | FieldRef::SpanId => base.with(Column::Trace),
            FieldRef::Message => base.with(Column::Msg),
            _ => ColumnSet::ALL,
        }
    }

    pub fn needs_body(&self) -> bool {
        self.columns().contains(storage::segment::Column::Body)
    }

    /// Path under which this field's values are Bloom-indexed, if any.
    pub fn bloom_path(&self, signal: Signal) -> Option<&str> {
        Some(match self {
            FieldRef::TraceId => paths::TRACE_ID,
            FieldRef::SpanId => paths::SPAN_ID,
            FieldRef::MessageTemplate => paths::MESSAGE_TEMPLATE,
            FieldRef::ExceptionType => paths::EXCEPTION_TYPE,
            // Only span and metric names are indexed; log messages are free
            // text and served by scanning.
            FieldRef::Message if signal != Signal::Logs => paths::NAME,
            FieldRef::ParentSpanId => paths::PARENT_SPAN_ID,
            FieldRef::Status => paths::STATUS,
            FieldRef::SpanKind => paths::SPAN_KIND,
            FieldRef::Attr(p) | FieldRef::AttrOnly(p) | FieldRef::Resource(p) => p,
            _ => return None,
        })
    }

    /// Path of this field's zone map (numeric min/max), if any.
    pub fn zone_path(&self) -> Option<&str> {
        Some(match self {
            FieldRef::DurationMs => paths::DURATION_MS,
            FieldRef::MetricValue => paths::VALUE,
            FieldRef::Attr(p) | FieldRef::AttrOnly(p) | FieldRef::Resource(p) => p,
            _ => return None,
        })
    }

    /// Display name for diagnostics.
    pub fn name(&self) -> String {
        match self {
            FieldRef::Timestamp => "timestamp".into(),
            FieldRef::Level => "level".into(),
            FieldRef::Service => "service".into(),
            FieldRef::Environment => "environment".into(),
            FieldRef::Message => "message".into(),
            FieldRef::TraceId => "traceId".into(),
            FieldRef::SpanId => "spanId".into(),
            FieldRef::MessageTemplate => "messageTemplate".into(),
            FieldRef::ExceptionType => "exception.type".into(),
            FieldRef::ExceptionMessage => "exception.message".into(),
            FieldRef::StackTrace => "exception.stackTrace".into(),
            FieldRef::ParentSpanId => "parentSpanId".into(),
            FieldRef::DurationMs => "durationMs".into(),
            FieldRef::Status => "status".into(),
            FieldRef::SpanKind => "spanKind".into(),
            FieldRef::MetricValue => "value".into(),
            FieldRef::Unit => "unit".into(),
            FieldRef::MetricKind => "metricKind".into(),
            FieldRef::Attr(p) => p.clone(),
            FieldRef::AttrOnly(p) => format!("attributes.{p}"),
            FieldRef::Resource(p) => format!("resource.{p}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_fields_are_case_insensitive() {
        assert_eq!(resolve("Level", Signal::Logs), FieldRef::Level);
        assert_eq!(resolve("SERVICE", Signal::Traces), FieldRef::Service);
        assert_eq!(resolve("traceId", Signal::Logs), FieldRef::TraceId);
        assert_eq!(resolve("@t", Signal::Logs), FieldRef::Timestamp);
    }

    #[test]
    fn signal_specific_fields() {
        assert_eq!(resolve("name", Signal::Traces), FieldRef::Message);
        assert_eq!(resolve("name", Signal::Logs), FieldRef::Attr("name".into()));
        assert_eq!(resolve("durationMs", Signal::Traces), FieldRef::DurationMs);
        assert_eq!(resolve("durationMs", Signal::Logs), FieldRef::Attr("durationMs".into()));
        assert_eq!(resolve("value", Signal::Metrics), FieldRef::MetricValue);
        assert_eq!(resolve("level", Signal::Traces), FieldRef::Attr("level".into()));
    }

    #[test]
    fn prefixes_and_properties() {
        assert_eq!(resolve("customerId", Signal::Logs), FieldRef::Attr("customerId".into()));
        assert_eq!(resolve("attributes.level", Signal::Logs), FieldRef::AttrOnly("level".into()));
        assert_eq!(resolve("properties.x.y", Signal::Logs), FieldRef::AttrOnly("x.y".into()));
        assert_eq!(resolve("resource.host.name", Signal::Logs), FieldRef::Resource("host.name".into()));
        // Property names stay case-sensitive.
        assert_eq!(resolve("CustomerId", Signal::Logs), FieldRef::Attr("CustomerId".into()));
    }

    #[test]
    fn column_requirements() {
        assert!(!resolve("level", Signal::Logs).needs_body());
        assert!(!resolve("message", Signal::Logs).needs_body());
        assert!(resolve("customerId", Signal::Logs).needs_body());
        assert!(resolve("level", Signal::Logs).columns().contains(Column::Meta));
    }
}
