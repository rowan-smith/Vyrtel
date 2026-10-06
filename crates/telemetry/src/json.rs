//! JSON representation used by the HTTP API and the web UI.
//!
//! This is a presentation format, not a storage format. Field names are
//! camelCase and empty optional fields are omitted.

use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::model::*;

impl Serialize for TelemetryEvent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("id", &self.id.to_string())?;
        m.serialize_entry(
            "signal",
            match self.payload {
                TelemetryPayload::Log(_) => "log",
                TelemetryPayload::Span(_) => "span",
                TelemetryPayload::Metric(_) => "metric",
            },
        )?;
        m.serialize_entry("timestamp", &self.timestamp)?;
        m.serialize_entry("observedTimestamp", &self.observed_timestamp)?;
        if let Some(v) = &self.service {
            m.serialize_entry("service", v)?;
        }
        if let Some(v) = &self.environment {
            m.serialize_entry("environment", v)?;
        }
        if let Some(v) = &self.trace_id {
            m.serialize_entry("traceId", v.as_str())?;
        }
        if let Some(v) = &self.span_id {
            m.serialize_entry("spanId", v.as_str())?;
        }

        match &self.payload {
            TelemetryPayload::Log(l) => {
                m.serialize_entry("level", l.level.as_str())?;
                m.serialize_entry("message", &l.message)?;
                if let Some(t) = &l.message_template {
                    m.serialize_entry("messageTemplate", t)?;
                }
                if let Some(e) = &l.exception {
                    m.serialize_entry("exception", &ExceptionJson(e))?;
                }
            }
            TelemetryPayload::Span(sp) => {
                m.serialize_entry("name", &sp.name)?;
                if let Some(p) = &sp.parent_span_id {
                    m.serialize_entry("parentSpanId", p.as_str())?;
                }
                m.serialize_entry("spanKind", sp.kind.as_str())?;
                m.serialize_entry("durationNanos", &sp.duration_nanos)?;
                m.serialize_entry("durationMs", &sp.duration_ms())?;
                m.serialize_entry("status", &StatusJson(&sp.status))?;
                if !sp.events.is_empty() {
                    let events: Vec<_> = sp.events.iter().map(SpanEventJson).collect();
                    m.serialize_entry("events", &events)?;
                }
                if !sp.links.is_empty() {
                    let links: Vec<_> = sp.links.iter().map(LinkJson).collect();
                    m.serialize_entry("links", &links)?;
                }
            }
            TelemetryPayload::Metric(mt) => {
                m.serialize_entry("name", &mt.name)?;
                if let Some(d) = &mt.description {
                    m.serialize_entry("description", d)?;
                }
                if let Some(u) = &mt.unit {
                    m.serialize_entry("unit", u)?;
                }
                m.serialize_entry("metricKind", mt.kind.as_str())?;
                m.serialize_entry("temporality", mt.temporality.as_str())?;
                m.serialize_entry("monotonic", &mt.monotonic)?;
                match &mt.value {
                    MetricValue::Number(v) => m.serialize_entry("value", v)?,
                    MetricValue::Histogram(h) => m.serialize_entry("histogram", &HistJson(h))?,
                }
            }
        }

        m.serialize_entry("properties", &self.attributes)?;
        m.serialize_entry("resource", &self.resource)?;
        m.end()
    }
}

struct ExceptionJson<'a>(&'a Exception);

impl Serialize for ExceptionJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        if let Some(v) = &self.0.kind {
            m.serialize_entry("type", v)?;
        }
        if let Some(v) = &self.0.message {
            m.serialize_entry("message", v)?;
        }
        if let Some(v) = &self.0.stack_trace {
            m.serialize_entry("stackTrace", v)?;
        }
        m.end()
    }
}

struct StatusJson<'a>(&'a SpanStatus);

impl Serialize for StatusJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("code", self.0.code.as_str())?;
        if let Some(msg) = &self.0.message {
            m.serialize_entry("message", msg)?;
        }
        m.end()
    }
}

struct SpanEventJson<'a>(&'a SpanEventItem);

impl Serialize for SpanEventJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("timestamp", &self.0.timestamp)?;
        m.serialize_entry("name", &self.0.name)?;
        m.serialize_entry("attributes", &self.0.attributes)?;
        m.end()
    }
}

struct LinkJson<'a>(&'a SpanLink);

impl Serialize for LinkJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("traceId", self.0.trace_id.as_str())?;
        m.serialize_entry("spanId", self.0.span_id.as_str())?;
        m.serialize_entry("attributes", &self.0.attributes)?;
        m.end()
    }
}

struct HistJson<'a>(&'a Histogram);

impl Serialize for HistJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let h = self.0;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("count", &h.count)?;
        if let Some(v) = h.sum {
            m.serialize_entry("sum", &v)?;
        }
        if let Some(v) = h.min {
            m.serialize_entry("min", &v)?;
        }
        if let Some(v) = h.max {
            m.serialize_entry("max", &v)?;
        }
        m.serialize_entry("bucketCounts", &h.bucket_counts)?;
        m.serialize_entry("explicitBounds", &h.explicit_bounds)?;
        m.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Fields, Timestamp, Value};

    #[test]
    fn log_json_shape() {
        let mut attrs = Fields::new();
        attrs.insert("customerId", Value::Int(481));
        let e = TelemetryEvent {
            id: EventId(7),
            timestamp: Timestamp::parse_rfc3339("2026-10-06T10:20:13.485Z").unwrap(),
            observed_timestamp: Timestamp::parse_rfc3339("2026-10-06T10:20:13.500Z").unwrap(),
            service: Some("payments".into()),
            environment: None,
            trace_id: TraceId::new("abc123"),
            span_id: None,
            resource: Fields::new(),
            attributes: attrs,
            payload: TelemetryPayload::Log(LogEvent {
                level: Level::Error,
                message: "Payment failed".into(),
                message_template: None,
                exception: Some(Exception {
                    kind: Some("TimeoutException".into()),
                    message: None,
                    stack_trace: Some("at x".into()),
                }),
            }),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["level"], "Error");
        assert_eq!(v["service"], "payments");
        assert_eq!(v["traceId"], "abc123");
        assert_eq!(v["properties"]["customerId"], 481);
        assert_eq!(v["exception"]["type"], "TimeoutException");
        assert_eq!(v["timestamp"], "2026-10-06T10:20:13.485Z");
        assert!(v.get("environment").is_none());
    }
}
