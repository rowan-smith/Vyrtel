//! OTLP/JSON decoding into the protobuf message structs.
//!
//! OTLP's JSON mapping differs from generic proto3 JSON in a few ways we
//! follow (https://opentelemetry.io/docs/specs/otlp/#json-protobuf-encoding):
//! trace/span ids are hex strings, 64-bit integers may be strings, enums are
//! integers. We are lenient where it is free: snake_case keys, enum names,
//! and numbers-as-strings are accepted too.

use base64::Engine as _;
use serde_json::{Map, Value as Json};

use super::proto::*;

type R<T> = Result<T, String>;

fn get<'a>(o: &'a Map<String, Json>, camel: &str, snake: &str) -> Option<&'a Json> {
    o.get(camel).or_else(|| o.get(snake)).filter(|v| !v.is_null())
}

fn obj<'a>(v: &'a Json, what: &str) -> R<&'a Map<String, Json>> {
    v.as_object().ok_or_else(|| format!("{what} must be an object"))
}

fn arr<'a>(o: &'a Map<String, Json>, camel: &str, snake: &str) -> R<&'a [Json]> {
    match get(o, camel, snake) {
        None => Ok(&[]),
        Some(Json::Array(a)) => Ok(a),
        Some(_) => Err(format!("{camel} must be an array")),
    }
}

fn string(o: &Map<String, Json>, camel: &str, snake: &str) -> String {
    match get(o, camel, snake) {
        Some(Json::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn u64_of(v: &Json) -> R<u64> {
    match v {
        Json::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
            .ok_or_else(|| format!("invalid unsigned integer {n}")),
        Json::String(s) => s.trim().parse().map_err(|_| format!("invalid unsigned integer '{s}'")),
        _ => Err("expected an integer".into()),
    }
}

fn i64_of(v: &Json) -> R<i64> {
    match v {
        Json::Number(n) => {
            n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).ok_or_else(|| format!("invalid integer {n}"))
        }
        Json::String(s) => s.trim().parse().map_err(|_| format!("invalid integer '{s}'")),
        _ => Err("expected an integer".into()),
    }
}

fn f64_of(v: &Json) -> R<f64> {
    match v {
        Json::Number(n) => n.as_f64().ok_or_else(|| "invalid number".into()),
        Json::String(s) => match s.as_str() {
            "NaN" => Ok(f64::NAN),
            "Infinity" => Ok(f64::INFINITY),
            "-Infinity" => Ok(f64::NEG_INFINITY),
            s => s.trim().parse().map_err(|_| format!("invalid number '{s}'")),
        },
        _ => Err("expected a number".into()),
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
        Some(Json::Number(n)) => n.as_i64().map(|i| i as i32).ok_or_else(|| format!("invalid {camel}")),
        Some(Json::String(s)) => names
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(s))
            .map(|(_, v)| *v)
            .or_else(|| s.parse().ok())
            .ok_or_else(|| format!("unknown {camel} '{s}'")),
        Some(_) => Err(format!("invalid {camel}")),
    }
}

/// Trace/span ids: hex per the OTLP JSON spec; base64 accepted as a
/// fallback for producers using generic proto3 JSON.
fn id_bytes(o: &Map<String, Json>, camel: &str, snake: &str) -> R<Vec<u8>> {
    let Some(v) = get(o, camel, snake) else {
        return Ok(Vec::new());
    };
    let s = v.as_str().ok_or_else(|| format!("{camel} must be a string"))?;
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(b) = hex::decode(s) {
        return Ok(b);
    }
    base64::engine::general_purpose::STANDARD.decode(s).map_err(|_| format!("{camel} must be hex encoded"))
}

pub fn any_value(v: &Json, depth: usize) -> R<AnyValue> {
    use any_value::Value as V;
    if depth > 32 {
        return Err("attribute value nested too deeply".into());
    }
    let o = obj(v, "AnyValue")?;
    let value = if let Some(s) = get(o, "stringValue", "string_value") {
        Some(V::StringValue(s.as_str().map(String::from).unwrap_or_else(|| s.to_string())))
    } else if let Some(b) = get(o, "boolValue", "bool_value") {
        Some(V::BoolValue(b.as_bool().ok_or("boolValue must be a boolean")?))
    } else if let Some(i) = get(o, "intValue", "int_value") {
        Some(V::IntValue(i64_of(i)?))
    } else if let Some(d) = get(o, "doubleValue", "double_value") {
        Some(V::DoubleValue(f64_of(d)?))
    } else if let Some(a) = get(o, "arrayValue", "array_value") {
        let a = obj(a, "arrayValue")?;
        Some(V::ArrayValue(ArrayValue {
            values: arr(a, "values", "values")?.iter().map(|x| any_value(x, depth + 1)).collect::<R<_>>()?,
        }))
    } else if let Some(kv) = get(o, "kvlistValue", "kvlist_value") {
        let kv = obj(kv, "kvlistValue")?;
        Some(V::KvlistValue(KeyValueList { values: key_values_depth(kv, "values", "values", depth + 1)? }))
    } else if let Some(b) = get(o, "bytesValue", "bytes_value") {
        let s = b.as_str().ok_or("bytesValue must be a string")?;
        Some(V::BytesValue(
            base64::engine::general_purpose::STANDARD.decode(s).map_err(|_| "bytesValue must be base64")?,
        ))
    } else {
        None
    };
    Ok(AnyValue { value })
}

fn key_values_depth(o: &Map<String, Json>, camel: &str, snake: &str, depth: usize) -> R<Vec<KeyValue>> {
    arr(o, camel, snake)?
        .iter()
        .map(|kv| {
            let kv = obj(kv, "KeyValue")?;
            Ok(KeyValue {
                key: string(kv, "key", "key"),
                value: get(kv, "value", "value").map(|v| any_value(v, depth)).transpose()?,
            })
        })
        .collect()
}

fn attributes(o: &Map<String, Json>) -> R<Vec<KeyValue>> {
    key_values_depth(o, "attributes", "attributes", 0)
}

fn resource(o: &Map<String, Json>) -> R<Option<Resource>> {
    get(o, "resource", "resource").map(|r| Ok(Resource { attributes: attributes(obj(r, "resource")?)? })).transpose()
}

fn scope(o: &Map<String, Json>) -> R<Option<InstrumentationScope>> {
    get(o, "scope", "scope")
        .map(|s| {
            let s = obj(s, "scope")?;
            Ok(InstrumentationScope {
                name: string(s, "name", "name"),
                version: string(s, "version", "version"),
                attributes: attributes(s)?,
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

fn root(body: &[u8]) -> R<Map<String, Json>> {
    match serde_json::from_slice::<Json>(body) {
        Ok(Json::Object(o)) => Ok(o),
        Ok(_) => Err("request must be a JSON object".into()),
        Err(e) => Err(format!("invalid JSON: {e}")),
    }
}

pub fn logs(body: &[u8]) -> R<ExportLogsServiceRequest> {
    let o = root(body)?;
    let mut req = ExportLogsServiceRequest::default();
    for rl in arr(&o, "resourceLogs", "resource_logs")? {
        let rl = obj(rl, "resourceLogs")?;
        let mut out = ResourceLogs { resource: resource(rl)?, scope_logs: Vec::new() };
        for sl in arr(rl, "scopeLogs", "scope_logs")? {
            let sl = obj(sl, "scopeLogs")?;
            let mut s = ScopeLogs { scope: scope(sl)?, log_records: Vec::new() };
            for lr in arr(sl, "logRecords", "log_records")? {
                let lr = obj(lr, "logRecord")?;
                s.log_records.push(LogRecord {
                    time_unix_nano: opt_u64(lr, "timeUnixNano", "time_unix_nano")?,
                    observed_time_unix_nano: opt_u64(lr, "observedTimeUnixNano", "observed_time_unix_nano")?,
                    severity_number: enum_of(lr, "severityNumber", "severity_number", SEVERITIES)?,
                    severity_text: string(lr, "severityText", "severity_text"),
                    body: get(lr, "body", "body").map(|b| any_value(b, 0)).transpose()?,
                    attributes: attributes(lr)?,
                    flags: opt_u64(lr, "flags", "flags")? as u32,
                    trace_id: id_bytes(lr, "traceId", "trace_id")?,
                    span_id: id_bytes(lr, "spanId", "span_id")?,
                    event_name: string(lr, "eventName", "event_name"),
                });
            }
            out.scope_logs.push(s);
        }
        req.resource_logs.push(out);
    }
    Ok(req)
}

pub fn traces(body: &[u8]) -> R<ExportTraceServiceRequest> {
    let o = root(body)?;
    let mut req = ExportTraceServiceRequest::default();
    for rs in arr(&o, "resourceSpans", "resource_spans")? {
        let rs = obj(rs, "resourceSpans")?;
        let mut out = ResourceSpans { resource: resource(rs)?, scope_spans: Vec::new() };
        for ss in arr(rs, "scopeSpans", "scope_spans")? {
            let ss = obj(ss, "scopeSpans")?;
            let mut s = ScopeSpans { scope: scope(ss)?, spans: Vec::new() };
            for sp in arr(ss, "spans", "spans")? {
                let sp = obj(sp, "span")?;
                let status = get(sp, "status", "status")
                    .map(|st| {
                        let st = obj(st, "status")?;
                        Ok::<_, String>(Status {
                            message: string(st, "message", "message"),
                            code: enum_of(st, "code", "code", STATUS_CODES)?,
                        })
                    })
                    .transpose()?;
                let mut events = Vec::new();
                for ev in arr(sp, "events", "events")? {
                    let ev = obj(ev, "event")?;
                    events.push(SpanEvent {
                        time_unix_nano: opt_u64(ev, "timeUnixNano", "time_unix_nano")?,
                        name: string(ev, "name", "name"),
                        attributes: attributes(ev)?,
                    });
                }
                let mut links = Vec::new();
                for l in arr(sp, "links", "links")? {
                    let l = obj(l, "link")?;
                    links.push(SpanLink {
                        trace_id: id_bytes(l, "traceId", "trace_id")?,
                        span_id: id_bytes(l, "spanId", "span_id")?,
                        attributes: attributes(l)?,
                    });
                }
                s.spans.push(Span {
                    trace_id: id_bytes(sp, "traceId", "trace_id")?,
                    span_id: id_bytes(sp, "spanId", "span_id")?,
                    trace_state: string(sp, "traceState", "trace_state"),
                    parent_span_id: id_bytes(sp, "parentSpanId", "parent_span_id")?,
                    name: string(sp, "name", "name"),
                    kind: enum_of(sp, "kind", "kind", SPAN_KINDS)?,
                    start_time_unix_nano: opt_u64(sp, "startTimeUnixNano", "start_time_unix_nano")?,
                    end_time_unix_nano: opt_u64(sp, "endTimeUnixNano", "end_time_unix_nano")?,
                    attributes: attributes(sp)?,
                    events,
                    links,
                    status,
                });
            }
            out.scope_spans.push(s);
        }
        req.resource_spans.push(out);
    }
    Ok(req)
}

fn number_point(p: &Map<String, Json>) -> R<NumberDataPoint> {
    use number_data_point::Value as V;
    let value = if let Some(d) = get(p, "asDouble", "as_double") {
        Some(V::AsDouble(f64_of(d)?))
    } else if let Some(i) = get(p, "asInt", "as_int") {
        Some(V::AsInt(i64_of(i)?))
    } else {
        None
    };
    Ok(NumberDataPoint {
        attributes: attributes(p)?,
        start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
        time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
        value,
    })
}

fn u64_list(p: &Map<String, Json>, camel: &str, snake: &str) -> R<Vec<u64>> {
    arr(p, camel, snake)?.iter().map(u64_of).collect()
}

pub fn metrics(body: &[u8]) -> R<ExportMetricsServiceRequest> {
    let o = root(body)?;
    let mut req = ExportMetricsServiceRequest::default();
    for rm in arr(&o, "resourceMetrics", "resource_metrics")? {
        let rm = obj(rm, "resourceMetrics")?;
        let mut out = ResourceMetrics { resource: resource(rm)?, scope_metrics: Vec::new() };
        for sm in arr(rm, "scopeMetrics", "scope_metrics")? {
            let sm = obj(sm, "scopeMetrics")?;
            let mut s = ScopeMetrics { scope: scope(sm)?, metrics: Vec::new() };
            for m in arr(sm, "metrics", "metrics")? {
                let m = obj(m, "metric")?;
                let data = if let Some(g) = get(m, "gauge", "gauge") {
                    let g = obj(g, "gauge")?;
                    Some(metric::Data::Gauge(Gauge {
                        data_points: arr(g, "dataPoints", "data_points")?
                            .iter()
                            .map(|p| number_point(obj(p, "dataPoint")?))
                            .collect::<R<_>>()?,
                    }))
                } else if let Some(su) = get(m, "sum", "sum") {
                    let su = obj(su, "sum")?;
                    Some(metric::Data::Sum(Sum {
                        data_points: arr(su, "dataPoints", "data_points")?
                            .iter()
                            .map(|p| number_point(obj(p, "dataPoint")?))
                            .collect::<R<_>>()?,
                        aggregation_temporality: enum_of(
                            su,
                            "aggregationTemporality",
                            "aggregation_temporality",
                            TEMPORALITIES,
                        )?,
                        is_monotonic: get(su, "isMonotonic", "is_monotonic").and_then(Json::as_bool).unwrap_or(false),
                    }))
                } else if let Some(h) = get(m, "histogram", "histogram") {
                    let h = obj(h, "histogram")?;
                    let mut points = Vec::new();
                    for p in arr(h, "dataPoints", "data_points")? {
                        let p = obj(p, "dataPoint")?;
                        points.push(HistogramDataPoint {
                            attributes: attributes(p)?,
                            start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
                            time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
                            count: opt_u64(p, "count", "count")?,
                            sum: opt_f64(p, "sum", "sum")?,
                            bucket_counts: u64_list(p, "bucketCounts", "bucket_counts")?,
                            explicit_bounds: arr(p, "explicitBounds", "explicit_bounds")?
                                .iter()
                                .map(f64_of)
                                .collect::<R<_>>()?,
                            min: opt_f64(p, "min", "min")?,
                            max: opt_f64(p, "max", "max")?,
                        });
                    }
                    Some(metric::Data::Histogram(Histogram {
                        data_points: points,
                        aggregation_temporality: enum_of(
                            h,
                            "aggregationTemporality",
                            "aggregation_temporality",
                            TEMPORALITIES,
                        )?,
                    }))
                } else if let Some(h) = get(m, "exponentialHistogram", "exponential_histogram") {
                    let h = obj(h, "exponentialHistogram")?;
                    let mut points = Vec::new();
                    for p in arr(h, "dataPoints", "data_points")? {
                        let p = obj(p, "dataPoint")?;
                        points.push(ExponentialHistogramDataPoint {
                            attributes: attributes(p)?,
                            start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
                            time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
                            count: opt_u64(p, "count", "count")?,
                            sum: opt_f64(p, "sum", "sum")?,
                            min: opt_f64(p, "min", "min")?,
                            max: opt_f64(p, "max", "max")?,
                        });
                    }
                    Some(metric::Data::ExponentialHistogram(ExponentialHistogram {
                        data_points: points,
                        aggregation_temporality: enum_of(
                            h,
                            "aggregationTemporality",
                            "aggregation_temporality",
                            TEMPORALITIES,
                        )?,
                    }))
                } else if let Some(su) = get(m, "summary", "summary") {
                    let su = obj(su, "summary")?;
                    let mut points = Vec::new();
                    for p in arr(su, "dataPoints", "data_points")? {
                        let p = obj(p, "dataPoint")?;
                        points.push(SummaryDataPoint {
                            attributes: attributes(p)?,
                            start_time_unix_nano: opt_u64(p, "startTimeUnixNano", "start_time_unix_nano")?,
                            time_unix_nano: opt_u64(p, "timeUnixNano", "time_unix_nano")?,
                            count: opt_u64(p, "count", "count")?,
                            sum: opt_f64(p, "sum", "sum")?.unwrap_or(0.0),
                        });
                    }
                    Some(metric::Data::Summary(Summary { data_points: points }))
                } else {
                    None
                };
                s.metrics.push(Metric {
                    name: string(m, "name", "name"),
                    description: string(m, "description", "description"),
                    unit: string(m, "unit", "unit"),
                    data,
                });
            }
            out.scope_metrics.push(s);
        }
        req.resource_metrics.push(out);
    }
    Ok(req)
}
