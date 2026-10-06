//! OTLP HTTP JSON conversion into the common Event model.
//! Supports the OTLP JSON encoding for logs and traces (no gRPC).

use chrono::{DateTime, TimeZone, Utc};
use event::{Event, EventType, LogLevel};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum OtlpError {
    #[error("invalid OTLP payload: {0}")]
    Invalid(String),
}

// --- Logs ---

#[derive(Debug, Deserialize)]
pub struct ExportLogsServiceRequest {
    #[serde(default, rename = "resourceLogs")]
    pub resource_logs: Vec<ResourceLogs>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceLogs {
    #[serde(default)]
    pub resource: Option<Resource>,
    #[serde(default, rename = "scopeLogs")]
    pub scope_logs: Vec<ScopeLogs>,
}

#[derive(Debug, Deserialize)]
pub struct Resource {
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeLogs {
    #[serde(default, rename = "logRecords")]
    pub log_records: Vec<LogRecord>,
}

#[derive(Debug, Deserialize)]
pub struct LogRecord {
    #[serde(default, rename = "timeUnixNano", deserialize_with = "de_u64_loose", alias = "observedTimeUnixNano")]
    pub time_unix_nano: Option<u64>,
    #[serde(default, rename = "severityNumber")]
    pub severity_number: Option<i32>,
    #[serde(default, rename = "severityText")]
    pub severity_text: Option<String>,
    #[serde(default)]
    pub body: Option<AnyValue>,
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
    #[serde(default, rename = "traceId")]
    pub trace_id: Option<String>,
    #[serde(default, rename = "spanId")]
    pub span_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KeyValue {
    pub key: String,
    #[serde(default)]
    pub value: Option<AnyValue>,
}

#[derive(Debug, Deserialize)]
pub struct AnyValue {
    #[serde(default, rename = "stringValue")]
    pub string_value: Option<String>,
    #[serde(default, rename = "boolValue")]
    pub bool_value: Option<bool>,
    #[serde(default, rename = "intValue", deserialize_with = "de_i64_loose")]
    pub int_value: Option<i64>,
    #[serde(default, rename = "doubleValue")]
    pub double_value: Option<f64>,
    #[serde(default, rename = "bytesValue")]
    pub bytes_value: Option<String>,
    #[serde(default, rename = "arrayValue")]
    pub array_value: Option<ArrayValue>,
    #[serde(default, rename = "kvlistValue")]
    pub kvlist_value: Option<KeyValueList>,
}

#[derive(Debug, Deserialize)]
pub struct ArrayValue {
    #[serde(default)]
    pub values: Vec<AnyValue>,
}

#[derive(Debug, Deserialize)]
pub struct KeyValueList {
    #[serde(default)]
    pub values: Vec<KeyValue>,
}

fn de_u64_loose<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?;
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_i64().map(|i| i as u64)),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    })
}

fn de_i64_loose<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(deserializer)?;
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    })
}

pub fn convert_logs(req: ExportLogsServiceRequest) -> Vec<Event> {
    let mut events = Vec::new();
    for rl in req.resource_logs {
        let (service, environment, mut resource_attrs) =
            extract_resource(&rl.resource.unwrap_or(Resource { attributes: vec![] }));
        for sl in rl.scope_logs {
            for record in sl.log_records {
                let mut event = Event::new_log();
                if let Some(nanos) = record.time_unix_nano {
                    event.timestamp = nanos_to_datetime(nanos);
                }
                event.level = record
                    .severity_text
                    .as_deref()
                    .and_then(LogLevel::parse)
                    .or_else(|| {
                        record
                            .severity_number
                            .map(LogLevel::from_otlp_severity_number)
                    });
                event.message = record.body.as_ref().and_then(any_value_to_string);
                event.service = service.clone();
                event.environment = environment.clone();
                event.trace_id = record.trace_id.filter(|s| !s.is_empty()).map(normalize_hex);
                event.span_id = record.span_id.filter(|s| !s.is_empty()).map(normalize_hex);

                let mut attrs = resource_attrs.clone();
                for kv in record.attributes {
                    if let Some(v) = kv.value.as_ref().and_then(any_value_to_json) {
                        match kv.key.as_str() {
                            "exception.stacktrace"
                            | "exception.stackTrace"
                            | "exception.stack_trace"
                            | "stacktrace"
                            | "stackTrace"
                            | "stack_trace"
                            | "error.stack"
                            | "error.stacktrace"
                            | "error.stack_trace"
                            | "@x" => {
                                if event.stacktrace.is_none() {
                                    event.stacktrace = match v {
                                        Value::String(s) => Some(s),
                                        other => Some(other.to_string()),
                                    };
                                }
                            }
                            _ => {
                                attrs.insert(kv.key, v);
                            }
                        }
                    }
                }
                event.attributes = attrs;
                events.push(event);
            }
        }
        let _ = &mut resource_attrs;
    }
    events
}

// --- Traces ---

#[derive(Debug, Deserialize)]
pub struct ExportTraceServiceRequest {
    #[serde(default, rename = "resourceSpans")]
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceSpans {
    #[serde(default)]
    pub resource: Option<Resource>,
    #[serde(default, rename = "scopeSpans")]
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeSpans {
    #[serde(default)]
    pub spans: Vec<OtlpSpan>,
}

#[derive(Debug, Deserialize)]
pub struct OtlpSpan {
    #[serde(default, rename = "traceId")]
    pub trace_id: Option<String>,
    #[serde(default, rename = "spanId")]
    pub span_id: Option<String>,
    #[serde(default, rename = "parentSpanId")]
    pub parent_span_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "startTimeUnixNano", deserialize_with = "de_u64_loose")]
    pub start_time_unix_nano: Option<u64>,
    #[serde(default, rename = "endTimeUnixNano", deserialize_with = "de_u64_loose")]
    pub end_time_unix_nano: Option<u64>,
    #[serde(default)]
    pub attributes: Vec<KeyValue>,
    #[serde(default)]
    pub status: Option<SpanStatus>,
}

#[derive(Debug, Deserialize)]
pub struct SpanStatus {
    #[serde(default)]
    pub code: Option<Value>,
    #[serde(default)]
    pub message: Option<String>,
}

pub fn convert_traces(req: ExportTraceServiceRequest) -> Vec<Event> {
    let mut events = Vec::new();
    for rs in req.resource_spans {
        let (service, environment, resource_attrs) =
            extract_resource(&rs.resource.unwrap_or(Resource { attributes: vec![] }));
        for ss in rs.scope_spans {
            for span in ss.spans {
                let mut event = Event::new_span();
                event.event_type = EventType::Span;
                if let Some(nanos) = span.start_time_unix_nano {
                    event.timestamp = nanos_to_datetime(nanos);
                }
                if let (Some(start), Some(end)) =
                    (span.start_time_unix_nano, span.end_time_unix_nano)
                {
                    if end >= start {
                        event.duration_ns = Some(end - start);
                    }
                }
                event.message = span.name;
                event.service = service.clone();
                event.environment = environment.clone();
                event.trace_id = span.trace_id.filter(|s| !s.is_empty()).map(normalize_hex);
                event.span_id = span.span_id.filter(|s| !s.is_empty()).map(normalize_hex);
                event.parent_span_id = span
                    .parent_span_id
                    .filter(|s| !s.is_empty())
                    .map(normalize_hex);

                let mut attrs = resource_attrs.clone();
                let mut stacktrace = None;
                for kv in span.attributes {
                    if let Some(v) = kv.value.as_ref().and_then(any_value_to_json) {
                        match kv.key.as_str() {
                            "exception.stacktrace"
                            | "exception.stackTrace"
                            | "exception.stack_trace"
                            | "stacktrace"
                            | "stackTrace"
                            | "stack_trace"
                            | "error.stack"
                            | "error.stacktrace"
                            | "error.stack_trace"
                            | "@x" => {
                                if stacktrace.is_none() {
                                    stacktrace = match v {
                                        Value::String(s) => Some(s),
                                        other => Some(other.to_string()),
                                    };
                                }
                            }
                            _ => {
                                attrs.insert(kv.key, v);
                            }
                        }
                    }
                }
                event.stacktrace = stacktrace;
                if let Some(status) = span.status {
                    if let Some(code) = status.code {
                        attrs.insert("otel.status_code".into(), code);
                    }
                    if let Some(msg) = status.message {
                        attrs.insert("otel.status_message".into(), Value::String(msg));
                    }
                }
                event.attributes = attrs;
                events.push(event);
            }
        }
    }
    events
}

fn extract_resource(resource: &Resource) -> (Option<String>, Option<String>, Map<String, Value>) {
    let mut service = None;
    let mut environment = None;
    let mut attrs = Map::new();
    for kv in &resource.attributes {
        let Some(v) = kv.value.as_ref().and_then(any_value_to_json) else {
            continue;
        };
        match kv.key.as_str() {
            "service.name" => service = v.as_str().map(|s| s.to_string()),
            "deployment.environment" | "deployment.environment.name" => {
                environment = v.as_str().map(|s| s.to_string())
            }
            _ => {
                attrs.insert(kv.key.clone(), v);
            }
        }
    }
    (service, environment, attrs)
}

fn any_value_to_string(v: &AnyValue) -> Option<String> {
    if let Some(s) = &v.string_value {
        return Some(s.clone());
    }
    any_value_to_json(v).map(|j| match j {
        Value::String(s) => s,
        other => other.to_string(),
    })
}

fn any_value_to_json(v: &AnyValue) -> Option<Value> {
    if let Some(s) = &v.string_value {
        return Some(Value::String(s.clone()));
    }
    if let Some(b) = v.bool_value {
        return Some(Value::Bool(b));
    }
    if let Some(i) = v.int_value {
        return Some(Value::Number(i.into()));
    }
    if let Some(d) = v.double_value {
        return serde_json::Number::from_f64(d).map(Value::Number);
    }
    if let Some(bytes) = &v.bytes_value {
        return Some(Value::String(bytes.clone()));
    }
    if let Some(arr) = &v.array_value {
        let values: Vec<Value> = arr
            .values
            .iter()
            .filter_map(any_value_to_json)
            .collect();
        return Some(Value::Array(values));
    }
    if let Some(kvlist) = &v.kvlist_value {
        let mut map = Map::new();
        for kv in &kvlist.values {
            if let Some(val) = kv.value.as_ref().and_then(any_value_to_json) {
                map.insert(kv.key.clone(), val);
            }
        }
        return Some(Value::Object(map));
    }
    None
}

fn nanos_to_datetime(nanos: u64) -> DateTime<Utc> {
    let secs = (nanos / 1_000_000_000) as i64;
    let nsecs = (nanos % 1_000_000_000) as u32;
    Utc.timestamp_opt(secs, nsecs)
        .single()
        .unwrap_or_else(Utc::now)
}

fn normalize_hex(s: String) -> String {
    // OTLP JSON may send base64 or hex; keep as-is if already hex-ish.
    if s.chars().all(|c| c.is_ascii_hexdigit()) {
        return s.to_ascii_lowercase();
    }
    // Try base64 decode → hex
    use base64::Engine;
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&s) {
        return hex_encode(&bytes);
    }
    s
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_log_record() {
        let json = r#"{
            "resourceLogs": [{
                "resource": {
                    "attributes": [
                        {"key": "service.name", "value": {"stringValue": "billing"}}
                    ]
                },
                "scopeLogs": [{
                    "logRecords": [{
                        "timeUnixNano": "1696500000000000000",
                        "severityText": "ERROR",
                        "body": {"stringValue": "Payment failed"},
                        "attributes": [
                            {"key": "customerId", "value": {"intValue": "42"}}
                        ],
                        "traceId": "abc123",
                        "spanId": "def456"
                    }]
                }]
            }]
        }"#;
        let req: ExportLogsServiceRequest = serde_json::from_str(json).unwrap();
        let events = convert_logs(req);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].service.as_deref(), Some("billing"));
        assert_eq!(events[0].level, Some(LogLevel::Error));
        assert_eq!(events[0].message.as_deref(), Some("Payment failed"));
        assert_eq!(events[0].attributes.get("customerId"), Some(&Value::Number(42.into())));
    }

    #[test]
    fn converts_log_record_with_exception_stacktrace() {
        let json = r#"{
            "resourceLogs": [{
                "resource": { "attributes": [] },
                "scopeLogs": [{
                    "logRecords": [{
                        "body": {"stringValue": "Unhandled exception"},
                        "attributes": [
                            {"key": "exception.stacktrace", "value": {"stringValue": "at MyApp.Run()"}},
                            {"key": "exception.type", "value": {"stringValue": "System.InvalidOperationException"}}
                        ]
                    }]
                }]
            }]
        }"#;
        let req: ExportLogsServiceRequest = serde_json::from_str(json).unwrap();
        let events = convert_logs(req);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].stacktrace.as_deref(), Some("at MyApp.Run()"));
        assert!(!events[0].attributes.contains_key("exception.stacktrace"));
        assert_eq!(
            events[0].attributes.get("exception.type"),
            Some(&Value::String("System.InvalidOperationException".into()))
        );
    }

    #[test]
    fn converts_span() {
        let json = r#"{
            "resourceSpans": [{
                "resource": {
                    "attributes": [
                        {"key": "service.name", "value": {"stringValue": "api"}}
                    ]
                },
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "aabbcc",
                        "spanId": "112233",
                        "parentSpanId": "",
                        "name": "GET /orders",
                        "startTimeUnixNano": "1000",
                        "endTimeUnixNano": "5000",
                        "attributes": []
                    }]
                }]
            }]
        }"#;
        let req: ExportTraceServiceRequest = serde_json::from_str(json).unwrap();
        let events = convert_traces(req);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::Span);
        assert_eq!(events[0].duration_ns, Some(4000));
        assert_eq!(events[0].message.as_deref(), Some("GET /orders"));
    }

    #[test]
    fn converts_span_with_exception_stacktrace() {
        let json = r#"{
            "resourceSpans": [{
                "resource": { "attributes": [] },
                "scopeSpans": [{
                    "spans": [{
                        "traceId": "aabbcc",
                        "spanId": "112233",
                        "name": "POST /checkout",
                        "attributes": [
                            {"key": "exception.stacktrace", "value": {"stringValue": "at DotnetLive.Services.InventoryService.ReserveAsync()"}}
                        ]
                    }]
                }]
            }]
        }"#;
        let req: ExportTraceServiceRequest = serde_json::from_str(json).unwrap();
        let events = convert_traces(req);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].stacktrace.as_deref(),
            Some("at DotnetLive.Services.InventoryService.ReserveAsync()")
        );
        assert!(!events[0].attributes.contains_key("exception.stacktrace"));
    }
}
