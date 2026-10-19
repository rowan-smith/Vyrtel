//! OTLP/JSON decoding into the protobuf message structs.
//!
//! OTLP's JSON mapping differs from generic proto3 JSON in a few ways we
//! follow (https://opentelemetry.io/docs/specs/otlp/#json-protobuf-encoding):
//! trace/span ids are hex strings, 64-bit integers may be strings, enums are
//! integers. We are lenient where it is free: snake_case keys, enum names,
//! and numbers-as-strings are accepted too.
//!
//! The body is parsed into a JSON tree and the structs are built from it,
//! all under the request's [`Budget`]: the tree as it is parsed, every
//! string and vector of the structs as it is made, each record counted as
//! it is reached. The tree is credited back once the structs are built.

use base64::Engine as _;
use serde_json::{Map, Value as Json};

use super::proto::*;
use crate::IngestError;
use crate::jsontree::{self, Ctx};
use crate::limits::{self, Budget};

type R<T> = Result<T, IngestError>;

fn bad(msg: impl std::fmt::Display) -> IngestError {
    IngestError::Malformed(format!("invalid OTLP payload: {msg}"))
}

fn get<'a>(o: &'a Map<String, Json>, camel: &str, snake: &str) -> Option<&'a Json> {
    o.get(camel).or_else(|| o.get(snake)).filter(|v| !v.is_null())
}

fn obj<'a>(v: &'a Json, what: &str) -> R<&'a Map<String, Json>> {
    v.as_object().ok_or_else(|| bad(format!("{what} must be an object")))
}

fn arr<'a>(o: &'a Map<String, Json>, camel: &str, snake: &str) -> R<&'a [Json]> {
    match get(o, camel, snake) {
        None => Ok(&[]),
        Some(Json::Array(a)) => Ok(a),
        Some(_) => Err(bad(format!("{camel} must be an array"))),
    }
}

fn string(o: &Map<String, Json>, camel: &str, snake: &str, b: &mut Budget) -> R<String> {
    let s = match get(o, camel, snake) {
        Some(Json::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    b.charge(s.capacity())?;
    Ok(s)
}

fn u64_of(v: &Json) -> R<u64> {
    match v {
        Json::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
            .ok_or_else(|| bad(format!("invalid unsigned integer {n}"))),
        Json::String(s) => s.trim().parse().map_err(|_| bad(format!("invalid unsigned integer '{s}'"))),
        _ => Err(bad("expected an integer")),
    }
}

fn i64_of(v: &Json) -> R<i64> {
    match v {
        Json::Number(n) => {
            n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).ok_or_else(|| bad(format!("invalid integer {n}")))
        }
        Json::String(s) => s.trim().parse().map_err(|_| bad(format!("invalid integer '{s}'"))),
        _ => Err(bad("expected an integer")),
    }
}

fn f64_of(v: &Json) -> R<f64> {
    match v {
        Json::Number(n) => n.as_f64().ok_or_else(|| bad("invalid number")),
        Json::String(s) => match s.as_str() {
            "NaN" => Ok(f64::NAN),
            "Infinity" => Ok(f64::INFINITY),
            "-Infinity" => Ok(f64::NEG_INFINITY),
            s => s.trim().parse().map_err(|_| bad(format!("invalid number '{s}'"))),
        },
        _ => Err(bad("expected a number")),
    }
}

fn opt_u64(o: &Map<String, Json>, camel: &str, snake: &str) -> R<u64> {
    get(o, camel, snake).map(u64_of).transpose().map(|v| v.unwrap_or(0))
}

fn opt_f64(o: &Map<String, Json>, camel: &str, snake: &str) -> R<Option<f64>> {
    get(o, camel, snake).map(f64_of).transpose()
}

/// Enum fields: integers, or their proto names (e.g. `SPAN_KIND_SERVER`).
fn enum_of(o: &Map<String, Json>, camel: &str, snake: &str, names: &[(&str, i32)]) -> R<i32> {
    match get(o, camel, snake) {
        None => Ok(0),
        Some(Json::Number(n)) => n.as_i64().map(|i| i as i32).ok_or_else(|| bad(format!("invalid {camel}"))),
        Some(Json::String(s)) => names
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(s))
            .map(|(_, v)| *v)
            .or_else(|| s.parse().ok())
            .ok_or_else(|| bad(format!("unknown {camel} '{s}'"))),
        Some(_) => Err(bad(format!("invalid {camel}"))),
    }
}

/// Trace/span ids: hex per the OTLP JSON spec; base64 accepted as a
/// fallback for producers using generic proto3 JSON.
fn id_bytes(o: &Map<String, Json>, camel: &str, snake: &str, b: &mut Budget) -> R<Vec<u8>> {
    let Some(v) = get(o, camel, snake) else {
        return Ok(Vec::new());
    };
    let s = v.as_str().ok_or_else(|| bad(format!("{camel} must be a string")))?;
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let id = match hex::decode(s) {
        Ok(id) => id,
        Err(_) => base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| bad(format!("{camel} must be hex encoded")))?,
    };
    b.charge(id.capacity())?;
    Ok(id)
}

/// An attribute value, nested `depth` levels below the attribute.
pub(crate) fn any_value(v: &Json, depth: usize, b: &mut Budget) -> R<AnyValue> {
    use any_value::Value as V;
    b.check_depth(depth)?;
    let o = obj(v, "AnyValue")?;
    let value = if let Some(s) = get(o, "stringValue", "string_value") {
        let s = s.as_str().map(String::from).unwrap_or_else(|| s.to_string());
        b.charge(s.capacity())?;
        Some(V::StringValue(s))
    } else if let Some(x) = get(o, "boolValue", "bool_value") {
        Some(V::BoolValue(x.as_bool().ok_or_else(|| bad("boolValue must be a boolean"))?))
    } else if let Some(i) = get(o, "intValue", "int_value") {
        Some(V::IntValue(i64_of(i)?))
    } else if let Some(d) = get(o, "doubleValue", "double_value") {
        Some(V::DoubleValue(f64_of(d)?))
    } else if let Some(a) = get(o, "arrayValue", "array_value") {
        let a = obj(a, "arrayValue")?;
        let mut values = Vec::new();
        for x in arr(a, "values", "values")? {
            let x = any_value(x, depth + 1, b)?;
            limits::push(&mut values, x, b)?;
        }
        Some(V::ArrayValue(ArrayValue { values }))
    } else if let Some(kv) = get(o, "kvlistValue", "kvlist_value") {
        let kv = obj(kv, "kvlistValue")?;
        Some(V::KvlistValue(KeyValueList { values: key_values_depth(kv, "values", "values", depth + 1, b)? }))
    } else if let Some(x) = get(o, "bytesValue", "bytes_value") {
        let s = x.as_str().ok_or_else(|| bad("bytesValue must be a string"))?;
        let bytes =
            base64::engine::general_purpose::STANDARD.decode(s).map_err(|_| bad("bytesValue must be base64"))?;
        b.charge(bytes.capacity())?;
        Some(V::BytesValue(bytes))
    } else {
        None
    };
    Ok(AnyValue { value })
}

fn key_values_depth(o: &Map<String, Json>, camel: &str, snake: &str, depth: usize, b: &mut Budget) -> R<Vec<KeyValue>> {
    let mut out = Vec::new();
    for kv in arr(o, camel, snake)? {
        let kv = obj(kv, "KeyValue")?;
        let kv = KeyValue {
            key: string(kv, "key", "key", b)?,
            value: get(kv, "value", "value").map(|v| any_value(v, depth, b)).transpose()?,
        };
        limits::push(&mut out, kv, b)?;
    }
    Ok(out)
}

fn attributes(o: &Map<String, Json>, b: &mut Budget) -> R<Vec<KeyValue>> {
    key_values_depth(o, "attributes", "attributes", 0, b)
}

fn resource(o: &Map<String, Json>, b: &mut Budget) -> R<Option<Resource>> {
    get(o, "resource", "resource").map(|r| Ok(Resource { attributes: attributes(obj(r, "resource")?, b)? })).transpose()
}

fn scope(o: &Map<String, Json>, b: &mut Budget) -> R<Option<InstrumentationScope>> {
    get(o, "scope", "scope")
        .map(|s| {
            let s = obj(s, "scope")?;
            Ok(InstrumentationScope {
                name: string(s, "name", "name", b)?,
                version: string(s, "version", "version", b)?,
                attributes: attributes(s, b)?,
            })
        })
        .transpose()
}

const SEVERITIES: &[(&str, i32)] = &[
    ("SEVERITY_NUMBER_UNSPECIFIED", 0),
    ("SEVERITY_NUMBER_TRACE", 1),
    ("SEVERITY_NUMBER_DEBUG", 5),
    ("SEVERITY_NUMBER_INFO", 9),
    ("SEVERITY_NUMBER_WARN", 13),
    ("SEVERITY_NUMBER_ERROR", 17),
    ("SEVERITY_NUMBER_FATAL", 21),
];

const SPAN_KINDS: &[(&str, i32)] = &[
    ("SPAN_KIND_UNSPECIFIED", 0),
    ("SPAN_KIND_INTERNAL", 1),
    ("SPAN_KIND_SERVER", 2),
    ("SPAN_KIND_CLIENT", 3),
    ("SPAN_KIND_PRODUCER", 4),
    ("SPAN_KIND_CONSUMER", 5),
];

const STATUS_CODES: &[(&str, i32)] = &[("STATUS_CODE_UNSET", 0), ("STATUS_CODE_OK", 1), ("STATUS_CODE_ERROR", 2)];

const TEMPORALITIES: &[(&str, i32)] = &[
    ("AGGREGATION_TEMPORALITY_UNSPECIFIED", 0),
    ("AGGREGATION_TEMPORALITY_DELTA", 1),
    ("AGGREGATION_TEMPORALITY_CUMULATIVE", 2),
];

/// Parse the body into a JSON tree under the budget, then build from it
/// with `build`; the tree is credited back once it is dropped.
fn decode<T>(body: &[u8], b: &mut Budget, build: impl FnOnce(&Map<String, Json>, &mut Budget) -> R<T>) -> R<T> {
    let before = b.used();
    // Each level of attribute value nesting is three or four levels of the
    // tree (`{"kvlistValue":{"values":[{"value":...}]}}`), so the tree is
    // left to serde_json's own limit of 128 levels, and values are held to
    // the depth limit while the structs are built.
    let tree = jsontree::from_slice(body, &mut Ctx::new(b, usize::MAX)).map_err(|e| match e {
        IngestError::Malformed(m) => bad(m),
        other => other,
    })?;
    let held = b.used() - before;
    let Json::Object(o) = tree else {
        return Err(bad("request must be a JSON object"));
    };
    let out = build(&o, b)?;
    drop(o);
    b.release(held);
    Ok(out)
}

pub(crate) fn logs(body: &[u8], b: &mut Budget) -> R<ExportLogsServiceRequest> {
    decode(body, b, |o, b| {
        let mut req = ExportLogsServiceRequest::default();
        for rl in arr(o, "resourceLogs", "resource_logs")? {
            let rl = obj(rl, "resourceLogs")?;
            let mut out = ResourceLogs { resource: resource(rl, b)?, scope_logs: Vec::new() };
            for sl in arr(rl, "scopeLogs", "scope_logs")? {
                let sl = obj(sl, "scopeLogs")?;
                let mut s = ScopeLogs { scope: scope(sl, b)?, log_records: Vec::new() };
                for lr in arr(sl, "logRecords", "log_records")? {
                    b.event()?;
                    let lr = obj(lr, "logRecord")?;
                    let record = LogRecord {
                        time_unix_nano: opt_u64(lr, "timeUnixNano", "time_unix_nano")?,
                        observed_time_unix_nano: opt_u64(lr, "observedTimeUnixNano", "observed_time_unix_nano")?,
                        severity_number: enum_of(lr, "severityNumber", "severity_number", SEVERITIES)?,
                        severity_text: string(lr, "severityText", "severity_text", b)?,
                        body: get(lr, "body", "body").map(|v| any_value(v, 0, b)).transpose()?,
                        attributes: attributes(lr, b)?,
                        flags: opt_u64(lr, "flags", "flags")? as u32,
                        trace_id: id_bytes(lr, "traceId", "trace_id", b)?,
                        span_id: id_bytes(lr, "spanId", "span_id", b)?,
                        event_name: string(lr, "eventName", "event_name", b)?,
                    };
                    limits::push(&mut s.log_records, record, b)?;
                }
                limits::push(&mut out.scope_logs, s, b)?;
            }
            limits::push(&mut req.resource_logs, out, b)?;
        }
        Ok(req)
    })
}

pub(crate) fn traces(body: &[u8], b: &mut Budget) -> R<ExportTraceServiceRequest> {
    decode(body, b, |o, b| {
        let mut req = ExportTraceServiceRequest::default();
        for rs in arr(o, "resourceSpans", "resource_spans")? {
            let rs = obj(rs, "resourceSpans")?;
            let mut out = ResourceSpans { resource: resource(rs, b)?, scope_spans: Vec::new() };
            for ss in arr(rs, "scopeSpans", "scope_spans")? {
                let ss = obj(ss, "scopeSpans")?;
                let mut s = ScopeSpans { scope: scope(ss, b)?, spans: Vec::new() };
                for sp in arr(ss, "spans", "spans")? {
                    b.event()?;
                    let sp = obj(sp, "span")?;
                    let status = get(sp, "status", "status")
                        .map(|st| {
                            let st = obj(st, "status")?;
                            Ok::<_, IngestError>(Status {
                                message: string(st, "message", "message", b)?,
                                code: enum_of(st, "code", "code", STATUS_CODES)?,
                            })
                        })
                        .transpose()?;
                    let mut events = Vec::new();
                    for ev in arr(sp, "events", "events")? {
                        let ev = obj(ev, "event")?;
                        let event = SpanEvent {
                            time_unix_nano: opt_u64(ev, "timeUnixNano", "time_unix_nano")?,
                            name: string(ev, "name", "name", b)?,
                            attributes: attributes(ev, b)?,
                        };
                        limits::push(&mut events, event, b)?;
                    }
                    let mut links = Vec::new();
                    for l in arr(sp, "links", "links")? {
                        let l = obj(l, "link")?;
                        let link = SpanLink {
                            trace_id: id_bytes(l, "traceId", "trace_id", b)?,
                            span_id: id_bytes(l, "spanId", "span_id", b)?,
                            attributes: attributes(l, b)?,
                        };
                        limits::push(&mut links, link, b)?;
                    }
                    let span = Span {
                        trace_id: id_bytes(sp, "traceId", "trace_id", b)?,
                        span_id: id_bytes(sp, "spanId", "span_id", b)?,
                        trace_state: string(sp, "traceState", "trace_state", b)?,
                        parent_span_id: id_bytes(sp, "parentSpanId", "parent_span_id", b)?,
                        name: string(sp, "name", "name", b)?,
                        kind: enum_of(sp, "kind", "kind", SPAN_KINDS)?,
                        start_time_unix_nano: opt_u64(sp, "startTimeUnixNano", "start_time_unix_nano")?,
                        end_time_unix_nano: opt_u64(sp, "endTimeUnixNano", "end_time_unix_nano")?,
                        attributes: attributes(sp, b)?,
                        events,
                        links,
                        status,
                    };
                    limits::push(&mut s.spans, span, b)?;
                }
                limits::push(&mut out.scope_spans, s, b)?;
            }
            limits::push(&mut req.resource_spans, out, b)?;
        }
        Ok(req)
    })
}

fn number_point(p: &Map<String, Json>, b: &mut Budget) -> R<NumberDataPoint> {
    use number_data_point::Value as V;
    let value = if let Some(d) = get(p, "asDouble", "as_double") {
        Some(V::AsDouble(f64_of(d)?))
    } else if let Some(i) = get(p, "asInt", "as_int") {
        Some(V::AsInt(i64_of(i)?))
    } else {
        None
    };
    Ok(NumberDataPoint {
        attributes: attributes(p, b)?,
        start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
        time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
        value,
    })
}

fn list<T>(p: &Map<String, Json>, camel: &str, snake: &str, of: fn(&Json) -> R<T>, b: &mut Budget) -> R<Vec<T>> {
    let mut out = Vec::new();
    for v in arr(p, camel, snake)? {
        limits::push(&mut out, of(v)?, b)?;
    }
    Ok(out)
}

/// Every data point of a metric, each one a record.
fn points<T>(
    m: &Map<String, Json>,
    what: &str,
    b: &mut Budget,
    point: impl Fn(&Map<String, Json>, &mut Budget) -> R<T>,
) -> R<Vec<T>> {
    let mut out = Vec::new();
    for p in arr(m, "dataPoints", "data_points")? {
        b.event()?;
        let p = point(obj(p, what)?, b)?;
        limits::push(&mut out, p, b)?;
    }
    Ok(out)
}

pub(crate) fn metrics(body: &[u8], b: &mut Budget) -> R<ExportMetricsServiceRequest> {
    decode(body, b, |o, b| {
        let mut req = ExportMetricsServiceRequest::default();
        for rm in arr(o, "resourceMetrics", "resource_metrics")? {
            let rm = obj(rm, "resourceMetrics")?;
            let mut out = ResourceMetrics { resource: resource(rm, b)?, scope_metrics: Vec::new() };
            for sm in arr(rm, "scopeMetrics", "scope_metrics")? {
                let sm = obj(sm, "scopeMetrics")?;
                let mut s = ScopeMetrics { scope: scope(sm, b)?, metrics: Vec::new() };
                for m in arr(sm, "metrics", "metrics")? {
                    let m = obj(m, "metric")?;
                    let metric = Metric {
                        name: string(m, "name", "name", b)?,
                        description: string(m, "description", "description", b)?,
                        unit: string(m, "unit", "unit", b)?,
                        data: metric_data(m, b)?,
                    };
                    limits::push(&mut s.metrics, metric, b)?;
                }
                limits::push(&mut out.scope_metrics, s, b)?;
            }
            limits::push(&mut req.resource_metrics, out, b)?;
        }
        Ok(req)
    })
}

fn metric_data(m: &Map<String, Json>, b: &mut Budget) -> R<Option<metric::Data>> {
    Ok(if let Some(g) = get(m, "gauge", "gauge") {
        let g = obj(g, "gauge")?;
        Some(metric::Data::Gauge(Gauge { data_points: points(g, "dataPoint", b, number_point)? }))
    } else if let Some(su) = get(m, "sum", "sum") {
        let su = obj(su, "sum")?;
        Some(metric::Data::Sum(Sum {
            data_points: points(su, "dataPoint", b, number_point)?,
            aggregation_temporality: enum_of(su, "aggregationTemporality", "aggregation_temporality", TEMPORALITIES)?,
            is_monotonic: get(su, "isMonotonic", "is_monotonic").and_then(Json::as_bool).unwrap_or(false),
        }))
    } else if let Some(h) = get(m, "histogram", "histogram") {
        let h = obj(h, "histogram")?;
        Some(metric::Data::Histogram(Histogram {
            data_points: points(h, "dataPoint", b, |p, b| {
                Ok(HistogramDataPoint {
                    attributes: attributes(p, b)?,
                    start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
                    time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
                    count: opt_u64(p, "count", "count")?,
                    sum: opt_f64(p, "sum", "sum")?,
                    bucket_counts: list(p, "bucketCounts", "bucket_counts", u64_of, b)?,
                    explicit_bounds: list(p, "explicitBounds", "explicit_bounds", f64_of, b)?,
                    min: opt_f64(p, "min", "min")?,
                    max: opt_f64(p, "max", "max")?,
                })
            })?,
            aggregation_temporality: enum_of(h, "aggregationTemporality", "aggregation_temporality", TEMPORALITIES)?,
        }))
    } else if let Some(h) = get(m, "exponentialHistogram", "exponential_histogram") {
        let h = obj(h, "exponentialHistogram")?;
        Some(metric::Data::ExponentialHistogram(ExponentialHistogram {
            data_points: points(h, "dataPoint", b, |p, b| {
                Ok(ExponentialHistogramDataPoint {
                    attributes: attributes(p, b)?,
                    start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
                    time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
                    count: opt_u64(p, "count", "count")?,
                    sum: opt_f64(p, "sum", "sum")?,
                    min: opt_f64(p, "min", "min")?,
                    max: opt_f64(p, "max", "max")?,
                })
            })?,
            aggregation_temporality: enum_of(h, "aggregationTemporality", "aggregation_temporality", TEMPORALITIES)?,
        }))
    } else if let Some(su) = get(m, "summary", "summary") {
        let su = obj(su, "summary")?;
        Some(metric::Data::Summary(Summary {
            data_points: points(su, "dataPoint", b, |p, b| {
                Ok(SummaryDataPoint {
                    attributes: attributes(p, b)?,
                    start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
                    time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
                    count: opt_u64(p, "count", "count")?,
                    sum: opt_f64(p, "sum", "sum")?.unwrap_or(0.0),
                })
            })?,
        }))
    } else {
        None
    })
}
