//! Native event format: JSON (single object, array, or `{"events": [...]}`)
//! and NDJSON (one object per line).
//!
//! Field names follow docs/api.md and accept common aliases, including
//! Serilog's compact format (CLEF: `@t`, `@l`, `@m`, `@mt`, `@x`, ...).
//! Unknown top-level fields become structured properties, so nothing a
//! producer sends is silently dropped.

use serde_json::{Map, Value as Json};
use telemetry::*;

use crate::IngestError;
use crate::render_template;

pub enum Format {
    Json,
    NdJson,
}

impl Format {
    pub fn from_content_type(ct: Option<&str>) -> Result<Format, IngestError> {
        let ct = ct.unwrap_or("application/json");
        let mime = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        match mime.as_str() {
            "" | "application/json" | "text/json" => Ok(Format::Json),
            "application/x-ndjson"
            | "application/ndjson"
            | "application/jsonl"
            | "application/x-jsonlines"
            | "application/vnd.serilog.clef" => Ok(Format::NdJson),
            other => Err(IngestError::UnsupportedContentType(other.to_string())),
        }
    }
}

/// Parse a request body into log events. Batches are all-or-nothing: one
/// invalid event rejects the request, with its index in the error.
pub fn parse(body: &[u8], format: Format, now: Timestamp) -> Result<Vec<TelemetryEvent>, IngestError> {
    let objects: Vec<Json> = match format {
        Format::Json => {
            let v: Json =
                serde_json::from_slice(body).map_err(|e| IngestError::Malformed(format!("invalid JSON: {e}")))?;
            match v {
                Json::Array(items) => items,
                Json::Object(mut o) if o.len() == 1 && o.get("events").is_some_and(Json::is_array) => {
                    match o.remove("events") {
                        Some(Json::Array(items)) => items,
                        _ => unreachable!(),
                    }
                }
                obj @ Json::Object(_) => vec![obj],
                _ => return Err(IngestError::Malformed("expected a JSON object or array of objects".into())),
            }
        }
        Format::NdJson => {
            let text =
                std::str::from_utf8(body).map_err(|_| IngestError::Malformed("body is not valid UTF-8".into()))?;
            let mut out = Vec::new();
            for (i, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let v: Json = serde_json::from_str(line)
                    .map_err(|e| IngestError::Malformed(format!("line {}: invalid JSON: {e}", i + 1)))?;
                out.push(v);
            }
            out
        }
    };
    if objects.is_empty() {
        return Err(IngestError::Malformed("no events in request".into()));
    }
    objects
        .into_iter()
        .enumerate()
        .map(|(index, v)| match v {
            Json::Object(o) => {
                event_from_object(o, now).map_err(|message| IngestError::InvalidEvent { index, message })
            }
            _ => Err(IngestError::InvalidEvent { index, message: "expected a JSON object".into() }),
        })
        .collect()
}

fn take(o: &mut Map<String, Json>, keys: &[&str]) -> Option<Json> {
    for k in keys {
        if let Some(v) = o.remove(*k)
            && !v.is_null()
        {
            return Some(v);
        }
    }
    None
}

fn text(v: Json) -> String {
    match v {
        Json::String(s) => s,
        other => other.to_string(),
    }
}

fn fields_of(v: Json, what: &str) -> Result<Fields, String> {
    match Value::from(v) {
        Value::Object(f) => Ok(f),
        other => Err(format!("'{what}' must be an object, not {}", other.type_name())),
    }
}

fn exception_from(v: Json) -> Option<Exception> {
    match v {
        Json::String(s) if !s.trim().is_empty() => {
            // A .NET/Java style stack trace: "Namespace.Type: message\n   at ..."
            let first = s.lines().next().unwrap_or("");
            let (kind, message) = match first.split_once(": ") {
                Some((k, m)) if !k.contains(' ') && !k.is_empty() => (Some(k.to_string()), Some(m.to_string())),
                _ => (None, None),
            };
            Some(Exception { kind, message, stack_trace: Some(s) })
        }
        Json::Object(mut o) => {
            let e = Exception {
                kind: take(&mut o, &["type", "Type", "kind", "class", "name"]).map(text),
                message: take(&mut o, &["message", "Message"]).map(text),
                stack_trace: take(&mut o, &["stackTrace", "stacktrace", "stack_trace", "StackTrace", "stack"])
                    .map(text),
            };
            (!e.is_empty()).then_some(e)
        }
        _ => None,
    }
}

fn event_from_object(mut o: Map<String, Json>, now: Timestamp) -> Result<TelemetryEvent, String> {
    let timestamp = match take(&mut o, &["timestamp", "@t", "time", "ts", "Timestamp"]) {
        None => now,
        Some(v) => parse_json_timestamp(&v).ok_or_else(|| format!("invalid timestamp {v}"))?,
    };
    let level_raw = take(&mut o, &["level", "@l", "severity", "severityText", "Level"]).map(text);
    let message = take(&mut o, &["message", "@m", "msg", "Message"]).map(text);
    let message_template = take(&mut o, &["messageTemplate", "@mt", "template", "MessageTemplate"]).map(text);
    let service = take(&mut o, &["service", "serviceName", "service.name", "Service"]).map(text);
    let environment = take(&mut o, &["environment", "env", "Environment"]).map(text);
    let trace_id = take(&mut o, &["traceId", "trace_id", "@tr", "TraceId"]).map(text);
    let span_id = take(&mut o, &["spanId", "span_id", "@sp", "SpanId"]).map(text);
    let exception = take(&mut o, &["exception", "@x", "Exception", "error"]).and_then(exception_from);

    let mut attributes = Fields::new();
    for key in ["properties", "attributes", "Properties"] {
        if let Some(v) = o.remove(key)
            && !v.is_null()
        {
            for (k, v) in fields_of(v, key)?.into_vec() {
                attributes.insert(k, v);
            }
        }
    }
    let resource = match o.remove("resource") {
        Some(Json::Null) | None => Fields::new(),
        Some(v) => fields_of(v, "resource")?,
    };
    // Everything else is a property (CLEF puts properties at top level).
    for (k, v) in o {
        attributes.insert(k, Value::from(v));
    }

    let level = match level_raw.as_deref() {
        None => Level::Information,
        Some(raw) => match Level::parse(raw) {
            Some(l) => l,
            None => {
                // Keep the producer's original spelling rather than lose it.
                attributes.insert("severityText", Value::from(raw));
                Level::Information
            }
        },
    };

    let service = service.or_else(|| resource.get("service.name").and_then(Value::as_str).map(String::from));
    let environment = environment.or_else(|| {
        resource
            .get("deployment.environment.name")
            .or_else(|| resource.get("deployment.environment"))
            .and_then(Value::as_str)
            .map(String::from)
    });

    let message = match (message, &message_template) {
        (Some(m), _) => m,
        (None, Some(t)) => render_template(t, &attributes),
        (None, None) => {
            exception.as_ref().and_then(|e| e.message.clone().or_else(|| e.kind.clone())).unwrap_or_default()
        }
    };
    Ok(TelemetryEvent {
        id: EventId(0),
        timestamp,
        observed_timestamp: now,
        service: service.filter(|s| !s.is_empty()),
        environment: environment.filter(|s| !s.is_empty()),
        trace_id: trace_id.and_then(|t| TraceId::new(&t)),
        span_id: span_id.and_then(|s| SpanId::new(&s)),
        resource,
        attributes,
        payload: TelemetryPayload::Log(LogEvent { level, message, message_template, exception }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp(1_000);

    fn one(json: &str) -> TelemetryEvent {
        let mut v = parse(json.as_bytes(), Format::Json, NOW).unwrap();
        assert_eq!(v.len(), 1);
        v.pop().unwrap()
    }

    #[test]
    fn spec_example_event() {
        let e = one(r#"{
              "timestamp": "2026-10-06T10:20:13.485Z",
              "level": "Error",
              "service": "payments",
              "environment": "production",
              "message": "Payment failed",
              "traceId": "abc123",
              "properties": {"customerId": 481, "paymentProvider": "stripe", "amount": 71.50},
              "exception": {"type": "TimeoutException", "message": "Payment provider timed out", "stackTrace": "..."}
            }"#);
        assert_eq!(e.timestamp, Timestamp::parse_rfc3339("2026-10-06T10:20:13.485Z").unwrap());
        assert_eq!(e.observed_timestamp, NOW);
        assert_eq!(e.level(), Some(Level::Error));
        assert_eq!(e.service.as_deref(), Some("payments"));
        assert_eq!(e.environment.as_deref(), Some("production"));
        assert_eq!(e.message(), "Payment failed");
        assert_eq!(e.trace_id.as_ref().unwrap().as_str(), "abc123");
        assert_eq!(e.attributes.get("customerId"), Some(&Value::Int(481)));
        assert_eq!(e.attributes.get("amount"), Some(&Value::Float(71.5)));
        let x = e.as_log().unwrap().exception.as_ref().unwrap();
        assert_eq!(x.kind.as_deref(), Some("TimeoutException"));
        assert_eq!(x.stack_trace.as_deref(), Some("..."));
    }

    #[test]
    fn clef_compatibility() {
        let e = one(
            r#"{"@t":"2026-01-01T00:00:00Z","@mt":"User {UserId} logged in","@l":"Warning","UserId":7,"@x":"System.InvalidOperationException: boom\n   at X.Y()"}"#,
        );
        assert_eq!(e.message(), "User 7 logged in");
        assert_eq!(e.as_log().unwrap().message_template.as_deref(), Some("User {UserId} logged in"));
        assert_eq!(e.level(), Some(Level::Warning));
        assert_eq!(e.attributes.get("UserId"), Some(&Value::Int(7)));
        let x = e.as_log().unwrap().exception.as_ref().unwrap();
        assert_eq!(x.kind.as_deref(), Some("System.InvalidOperationException"));
        assert_eq!(x.message.as_deref(), Some("boom"));
    }

    #[test]
    fn defaults_and_unknown_levels() {
        let e = one(r#"{"message": "hi"}"#);
        assert_eq!(e.timestamp, NOW);
        assert_eq!(e.level(), Some(Level::Information));
        let e = one(r#"{"message": "hi", "level": "notice-ish"}"#);
        assert_eq!(e.level(), Some(Level::Information));
        assert_eq!(e.attributes.get("severityText"), Some(&Value::from("notice-ish")));
    }

    #[test]
    fn batches_and_ndjson() {
        let v = parse(br#"[{"message":"a"},{"message":"b"}]"#, Format::Json, NOW).unwrap();
        assert_eq!(v.len(), 2);
        let v = parse(br#"{"events":[{"message":"a"}]}"#, Format::Json, NOW).unwrap();
        assert_eq!(v[0].message(), "a");
        let v = parse(b"{\"message\":\"a\"}\n\n{\"message\":\"b\"}\r\n", Format::NdJson, NOW).unwrap();
        assert_eq!(v.iter().map(|e| e.message()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn errors() {
        assert!(matches!(parse(b"not json", Format::Json, NOW), Err(IngestError::Malformed(_))));
        assert!(matches!(parse(b"[]", Format::Json, NOW), Err(IngestError::Malformed(_))));
        assert!(matches!(parse(b"42", Format::Json, NOW), Err(IngestError::Malformed(_))));
        assert_eq!(
            parse(br#"[{"message":"ok"}, 5]"#, Format::Json, NOW),
            Err(IngestError::InvalidEvent { index: 1, message: "expected a JSON object".into() })
        );
        assert!(matches!(
            parse(br#"{"timestamp": "yesterday"}"#, Format::Json, NOW),
            Err(IngestError::InvalidEvent { index: 0, .. })
        ));
        assert!(matches!(
            parse(br#"{"properties": [1]}"#, Format::Json, NOW),
            Err(IngestError::InvalidEvent { index: 0, .. })
        ));
        let e = parse(b"{\"a\":1}\n{oops", Format::NdJson, NOW).unwrap_err();
        assert!(e.to_string().starts_with("line 2"));
        assert!(matches!(Format::from_content_type(Some("text/csv")), Err(IngestError::UnsupportedContentType(_))));
        assert!(matches!(Format::from_content_type(Some("application/x-ndjson; charset=utf-8")), Ok(Format::NdJson)));
    }

    #[test]
    fn unknown_fields_become_properties_and_resource_fills_service() {
        let e =
            one(r#"{"message":"m","region":"eu","resource":{"service.name":"api","deployment.environment":"prod"}}"#);
        assert_eq!(e.attributes.get("region"), Some(&Value::from("eu")));
        assert_eq!(e.service.as_deref(), Some("api"));
        assert_eq!(e.environment.as_deref(), Some("prod"));
        assert_eq!(e.resource.get("service.name"), Some(&Value::from("api")));
    }

    #[test]
    fn messy_types_are_kept() {
        let v = parse(
            br#"[{"message":"a","x":1},{"message":"b","x":"one"},{"message":"c","x":[1,{"y":null}]}]"#,
            Format::Json,
            NOW,
        )
        .unwrap();
        assert_eq!(v[0].attributes.get("x"), Some(&Value::Int(1)));
        assert_eq!(v[1].attributes.get("x"), Some(&Value::from("one")));
        assert!(matches!(v[2].attributes.get("x"), Some(Value::Array(_))));
    }

    #[test]
    fn numeric_timestamps() {
        let e = one(r#"{"message":"m","timestamp":1791282013485}"#);
        assert_eq!(e.timestamp, Timestamp::from_millis(1_791_282_013_485));
    }
}
