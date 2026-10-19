//! REL-16: per-request decoding limits, through the public decoders.
//!
//! * TC-01 at the decoder: every limit at limit − 1, limit and limit + 1,
//!   for every format and encoding (gzip is HTTP's and is covered by the
//!   server's integration tests);
//! * AC3: records of one OTLP resource share it;
//! * the protobuf pre-scan's estimate covers what `prost` allocates, and it
//!   accepts what `prost` accepts.

use std::sync::Arc;

use ingest::otlp::proto::*;
use ingest::otlp::{self, Encoding, scan, size};
use ingest::{IngestError, Limits, MAX_DEPTH, native};
use prost::Message;
use telemetry::{TelemetryEvent, Timestamp};

const NOW: Timestamp = Timestamp(1);

#[derive(Clone, Copy, Debug)]
enum Format {
    NativeArray,
    NativeEnvelope,
    NdJson,
    Logs(Encoding),
    Traces(Encoding),
    Metrics(Encoding),
}

const ALL: [Format; 9] = [
    Format::NativeArray,
    Format::NativeEnvelope,
    Format::NdJson,
    Format::Logs(Encoding::Protobuf),
    Format::Logs(Encoding::Json),
    Format::Traces(Encoding::Protobuf),
    Format::Traces(Encoding::Json),
    Format::Metrics(Encoding::Protobuf),
    Format::Metrics(Encoding::Json),
];

fn decode(f: Format, body: &[u8], limits: Limits) -> Result<Vec<TelemetryEvent>, IngestError> {
    match f {
        Format::NativeArray | Format::NativeEnvelope => native::parse(body, native::Format::Json, NOW, limits),
        Format::NdJson => native::parse(body, native::Format::NdJson, NOW, limits),
        Format::Logs(e) => otlp::decode_logs(body, e, NOW, limits).map(|m| m.events),
        Format::Traces(e) => otlp::decode_traces(body, e, NOW, limits).map(|m| m.events),
        Format::Metrics(e) => otlp::decode_metrics(body, e, NOW, limits).map(|m| m.events),
    }
}

fn s(v: impl Into<String>) -> Option<AnyValue> {
    Some(AnyValue { value: Some(any_value::Value::StringValue(v.into())) })
}

fn kv(k: &str, value: Option<AnyValue>) -> KeyValue {
    KeyValue { key: k.into(), value }
}

fn resource(name: &str) -> Option<Resource> {
    Some(Resource { attributes: vec![kv("service.name", s(name)), kv("host.name", s("web-1"))] })
}

fn scope() -> Option<InstrumentationScope> {
    Some(InstrumentationScope { name: "lib".into(), ..Default::default() })
}

/// `value` as an OTLP/JSON `AnyValue` (strings and key/value lists only).
fn any_json(v: &Option<AnyValue>) -> String {
    match v.as_ref().and_then(|v| v.value.as_ref()) {
        Some(any_value::Value::StringValue(s)) => format!(r#"{{"stringValue":{}}}"#, serde_json::to_string(s).unwrap()),
        Some(any_value::Value::KvlistValue(l)) => format!(r#"{{"kvlistValue":{{"values":{}}}}}"#, kvs_json(&l.values)),
        Some(any_value::Value::ArrayValue(a)) => {
            let values: Vec<String> = a.values.iter().map(|v| any_json(&Some(v.clone()))).collect();
            format!(r#"{{"arrayValue":{{"values":[{}]}}}}"#, values.join(","))
        }
        _ => "{}".into(),
    }
}

fn kvs_json(kvs: &[KeyValue]) -> String {
    let items: Vec<String> =
        kvs.iter().map(|kv| format!(r#"{{"key":{:?},"value":{}}}"#, kv.key, any_json(&kv.value))).collect();
    format!("[{}]", items.join(","))
}

fn log_record(i: usize, attributes: Vec<KeyValue>) -> LogRecord {
    LogRecord {
        time_unix_nano: 1_700_000_000_000_000_000 + i as u64,
        severity_number: 9,
        body: s(format!("record {i}")),
        attributes,
        trace_id: vec![7; 16],
        span_id: vec![8; 8],
        ..Default::default()
    }
}

/// `n` log records with `attributes` each, under two resources.
fn logs(enc: Encoding, n: usize, attributes: impl Fn() -> Vec<KeyValue>) -> Vec<u8> {
    let split = n / 2;
    let req = ExportLogsServiceRequest {
        resource_logs: [("a", 0..split), ("b", split..n)]
            .into_iter()
            .map(|(name, range)| ResourceLogs {
                resource: resource(name),
                scope_logs: vec![ScopeLogs {
                    scope: scope(),
                    log_records: range.map(|i| log_record(i, attributes())).collect(),
                }],
            })
            .collect(),
    };
    match enc {
        Encoding::Protobuf => req.encode_to_vec(),
        Encoding::Json => {
            let rl: Vec<String> = req
                .resource_logs
                .iter()
                .map(|rl| {
                    let records: Vec<String> = rl.scope_logs[0]
                        .log_records
                        .iter()
                        .map(|lr| {
                            format!(
                                r#"{{"timeUnixNano":"{}","severityNumber":9,"body":{},"attributes":{},"traceId":"{}","spanId":"{}"}}"#,
                                lr.time_unix_nano,
                                any_json(&lr.body),
                                kvs_json(&lr.attributes),
                                hex::encode(&lr.trace_id),
                                hex::encode(&lr.span_id),
                            )
                        })
                        .collect();
                    format!(
                        r#"{{"resource":{{"attributes":{}}},"scopeLogs":[{{"scope":{{"name":"lib"}},"logRecords":[{}]}}]}}"#,
                        kvs_json(&rl.resource.as_ref().unwrap().attributes),
                        records.join(","),
                    )
                })
                .collect();
            format!(r#"{{"resourceLogs":[{}]}}"#, rl.join(",")).into_bytes()
        }
    }
}

fn traces(enc: Encoding, n: usize) -> Vec<u8> {
    let spans: Vec<Span> = (0..n)
        .map(|i| Span {
            trace_id: vec![1; 16],
            span_id: vec![(i % 250) as u8 + 1; 8],
            name: format!("GET /{i}"),
            kind: 2,
            start_time_unix_nano: 1_000 + i as u64,
            end_time_unix_nano: 2_000 + i as u64,
            attributes: vec![kv("http.route", s("/x"))],
            ..Default::default()
        })
        .collect();
    match enc {
        Encoding::Protobuf => ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: resource("api"),
                scope_spans: vec![ScopeSpans { scope: scope(), spans }],
            }],
        }
        .encode_to_vec(),
        Encoding::Json => {
            let spans: Vec<String> = spans
                .iter()
                .map(|sp| {
                    format!(
                        r#"{{"traceId":"{}","spanId":"{}","name":{:?},"kind":2,"startTimeUnixNano":"{}","endTimeUnixNano":"{}","attributes":{}}}"#,
                        hex::encode(&sp.trace_id),
                        hex::encode(&sp.span_id),
                        sp.name,
                        sp.start_time_unix_nano,
                        sp.end_time_unix_nano,
                        kvs_json(&sp.attributes),
                    )
                })
                .collect();
            format!(
                r#"{{"resourceSpans":[{{"resource":{{"attributes":{}}},"scopeSpans":[{{"scope":{{"name":"lib"}},"spans":[{}]}}]}}]}}"#,
                kvs_json(&resource("api").unwrap().attributes),
                spans.join(","),
            )
            .into_bytes()
        }
    }
}

/// `n` data points: gauges of one point each, every third a histogram.
fn metrics(enc: Encoding, n: usize) -> Vec<u8> {
    let metrics: Vec<Metric> = (0..n)
        .map(|i| {
            let data = if i % 3 == 2 {
                metric::Data::Histogram(Histogram {
                    data_points: vec![HistogramDataPoint {
                        attributes: vec![kv("host", s("a"))],
                        time_unix_nano: 5,
                        count: 3,
                        bucket_counts: vec![1, 2],
                        explicit_bounds: vec![10.0],
                        ..Default::default()
                    }],
                    aggregation_temporality: 2,
                })
            } else {
                metric::Data::Gauge(Gauge {
                    data_points: vec![NumberDataPoint {
                        attributes: vec![kv("host", s("a"))],
                        time_unix_nano: 5,
                        value: Some(number_data_point::Value::AsDouble(i as f64)),
                        ..Default::default()
                    }],
                })
            };
            Metric { name: format!("m{i}"), unit: "1".into(), data: Some(data), ..Default::default() }
        })
        .collect();
    match enc {
        Encoding::Protobuf => ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: resource("api"),
                scope_metrics: vec![ScopeMetrics { scope: scope(), metrics }],
            }],
        }
        .encode_to_vec(),
        Encoding::Json => {
            let ms: Vec<String> = metrics
                .iter()
                .map(|m| match m.data.as_ref().unwrap() {
                    metric::Data::Histogram(_) => format!(
                        r#"{{"name":{:?},"unit":"1","histogram":{{"aggregationTemporality":2,"dataPoints":[{{"attributes":{},"timeUnixNano":"5","count":"3","bucketCounts":["1","2"],"explicitBounds":[10.0]}}]}}}}"#,
                        m.name,
                        kvs_json(&[kv("host", s("a"))]),
                    ),
                    _ => format!(
                        r#"{{"name":{:?},"unit":"1","gauge":{{"dataPoints":[{{"attributes":{},"timeUnixNano":"5","asDouble":1.5}}]}}}}"#,
                        m.name,
                        kvs_json(&[kv("host", s("a"))]),
                    ),
                })
                .collect();
            format!(
                r#"{{"resourceMetrics":[{{"resource":{{"attributes":{}}},"scopeMetrics":[{{"scope":{{"name":"lib"}},"metrics":[{}]}}]}}]}}"#,
                kvs_json(&resource("api").unwrap().attributes),
                ms.join(","),
            )
            .into_bytes()
        }
    }
}

fn native_event(i: usize, properties: &str) -> String {
    format!(r#"{{"message":"event {i}","level":"Warning","service":"api","properties":{properties}}}"#)
}

/// A body of `n` records in format `f`, each carrying `properties` (native)
/// or the equivalent attributes (OTLP logs).
fn body(f: Format, n: usize) -> Vec<u8> {
    let props = r#"{"customerId":481,"region":"eu"}"#;
    let events = || (0..n).map(|i| native_event(i, props)).collect::<Vec<_>>();
    match f {
        Format::NativeArray => format!("[{}]", events().join(",")).into_bytes(),
        Format::NativeEnvelope => format!(r#"{{"events":[{}]}}"#, events().join(",")).into_bytes(),
        Format::NdJson => events().join("\n").into_bytes(),
        Format::Logs(e) => logs(e, n, || vec![kv("customerId", s("481")), kv("region", s("eu"))]),
        Format::Traces(e) => traces(e, n),
        Format::Metrics(e) => metrics(e, n),
    }
}

fn with_events(max_events: usize) -> Limits {
    Limits { max_events, ..Limits::UNBOUNDED }
}

fn with_bytes(max_decoded_bytes: usize) -> Limits {
    Limits { max_decoded_bytes, ..Limits::UNBOUNDED }
}

// TC-01 / AC2: the event limit, exactly.
#[test]
fn event_limit_is_exact_for_every_format() {
    for f in ALL {
        let n = 12;
        let b = body(f, n);
        assert_eq!(decode(f, &b, with_events(n - 1)).err(), Some(IngestError::TooManyEvents { limit: n - 1 }), "{f:?}");
        for limit in [n, n + 1] {
            assert_eq!(decode(f, &b, with_events(limit)).unwrap().len(), n, "{f:?} at {limit}");
        }
    }
}

/// The smallest byte limit `b` decodes within.
fn smallest_byte_limit(f: Format, b: &[u8]) -> usize {
    let (mut lo, mut hi) = (0, 64 * b.len() + (1 << 20));
    assert!(decode(f, b, with_bytes(hi)).is_ok(), "{f:?} decodes within {hi}");
    while lo < hi {
        let mid = (lo + hi) / 2;
        if decode(f, b, with_bytes(mid)).is_ok() {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

// TC-01 / AC1: the decoded-bytes limit, exactly. (Which real batches fit
// the server's allowance is the amplification test's subject.)
#[test]
fn byte_limit_is_exact_for_every_format() {
    for f in ALL {
        let n = 40;
        let b = body(f, n);
        let limit = smallest_byte_limit(f, &b);
        assert_eq!(
            decode(f, &b, with_bytes(limit - 1)).err(),
            Some(IngestError::TooLarge { limit_bytes: limit - 1 }),
            "{f:?}"
        );
        for l in [limit, limit + 1] {
            assert_eq!(decode(f, &b, with_bytes(l)).unwrap().len(), n, "{f:?} at {l}");
        }
    }
}

// Both limits are checked before the batch is built: a body with far more
// events than allowed, or that cannot fit, fails on its count alone.
#[test]
fn oversized_batches_fail_before_decoding() {
    for f in ALL {
        let b = body(f, 2_000);
        assert_eq!(decode(f, &b, with_events(10)).err(), Some(IngestError::TooManyEvents { limit: 10 }), "{f:?}");
        assert_eq!(decode(f, &b, with_bytes(1024)).err(), Some(IngestError::TooLarge { limit_bytes: 1024 }), "{f:?}");
    }
}

/// `depth` nested arrays around a number.
fn nested_json(depth: usize) -> String {
    format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
}

/// An attribute value of `depth` arrays nested around a string.
fn nested_value(depth: usize) -> Option<AnyValue> {
    (0..depth).fold(s("leaf"), |inner, _| {
        Some(AnyValue { value: Some(any_value::Value::ArrayValue(ArrayValue { values: vec![inner.unwrap()] })) })
    })
}

/// An attribute value of `depth` key/value lists nested around a string.
fn nested_list(depth: usize) -> Option<AnyValue> {
    (0..depth).fold(s("leaf"), |inner, _| {
        Some(AnyValue { value: Some(any_value::Value::KvlistValue(KeyValueList { values: vec![kv("k", inner)] })) })
    })
}

// TC-01: nesting, at the limit and one past it (one level below the limit
// is covered by every other test).
#[test]
fn depth_limit_is_exact() {
    let too_deep = Err(IngestError::TooDeep { limit: MAX_DEPTH });
    // Native: a property holding `depth` nested arrays sits at depth 1 of
    // the event, so its innermost array is at `depth`.
    for (depth, ok) in [(MAX_DEPTH, true), (MAX_DEPTH + 1, false)] {
        let event = native_event(0, &format!(r#"{{"p":{}}}"#, nested_json(depth - 1)));
        for (f, body) in [
            (Format::NativeArray, format!("[{event}]")),
            (Format::NativeEnvelope, format!(r#"{{"events":[{event}]}}"#)),
            (Format::NativeArray, event.clone()),
            (Format::NdJson, event.clone()),
        ] {
            let r = decode(f, body.as_bytes(), Limits::UNBOUNDED).map(|v| v.len());
            assert_eq!(r.is_ok(), ok, "{f:?} at depth {depth}: {r:?}");
            if !ok {
                assert_eq!(r, too_deep.clone().map(|()| 0), "{f:?}");
            }
        }
    }
    // OTLP: the attribute value is at depth 0, so `depth` arrays put the
    // innermost value at `depth`.
    for (depth, ok) in [(MAX_DEPTH, true), (MAX_DEPTH + 1, false), (3 * MAX_DEPTH, false)] {
        for enc in [Encoding::Protobuf, Encoding::Json] {
            let f = Format::Logs(enc);
            let b = logs(enc, 2, || vec![kv("deep", nested_value(depth))]);
            let r = decode(f, &b, Limits::UNBOUNDED).map(|v| v.len());
            assert_eq!(r.is_ok(), ok, "{f:?} at depth {depth}: {r:?}");
            if !ok {
                assert_eq!(r, too_deep.clone().map(|()| 0), "{f:?} at depth {depth}");
            }
        }
    }
    // Key/value lists take several levels each of the JSON tree and of
    // `prost`'s message nesting, whose own limits (128 and 100) are reached
    // first; that is reported the same way.
    for enc in [Encoding::Protobuf, Encoding::Json] {
        let b = logs(enc, 2, || vec![kv("deep", nested_list(MAX_DEPTH))]);
        assert_eq!(decode(Format::Logs(enc), &b, Limits::UNBOUNDED).map(|v| v.len()), too_deep.clone().map(|()| 0));
        let b = logs(enc, 2, || vec![kv("deep", nested_list(24))]);
        assert!(decode(Format::Logs(enc), &b, Limits::UNBOUNDED).is_ok(), "{enc:?}");
    }
}

// AC3: one resource, shared by all of its records rather than copied.
#[test]
fn records_share_their_resource() {
    for enc in [Encoding::Protobuf, Encoding::Json] {
        let events = decode(Format::Logs(enc), &body(Format::Logs(enc), 6), Limits::UNBOUNDED).unwrap();
        let (a, b) = events.split_at(3);
        assert!(a.iter().all(|e| Arc::ptr_eq(&e.resource, &a[0].resource)), "{enc:?}");
        assert!(b.iter().all(|e| Arc::ptr_eq(&e.resource, &b[0].resource)), "{enc:?}");
        assert!(!Arc::ptr_eq(&a[0].resource, &b[0].resource));
        assert_eq!(a[0].service.as_deref(), Some("a"));
        assert_eq!(b[0].resource.get("service.name"), Some(&telemetry::Value::from("b")));
    }
    // Native events without a resource hold none of their own.
    let events = decode(Format::NdJson, &body(Format::NdJson, 2), Limits::UNBOUNDED).unwrap();
    assert!(Arc::ptr_eq(&events[0].resource, &events[1].resource));
}

// A large resource costs once per request, whatever the record count.
#[test]
fn resource_size_does_not_multiply_by_record_count() {
    let big = || {
        let mut r = resource("api").unwrap();
        r.attributes.extend((0..200).map(|k| kv(&format!("attr.{k}"), s("x".repeat(24)))));
        Some(r)
    };
    let need = |n: usize| {
        let req = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: big(),
                scope_logs: vec![ScopeLogs {
                    scope: scope(),
                    log_records: (0..n).map(|i| log_record(i, vec![])).collect(),
                }],
            }],
        };
        smallest_byte_limit(Format::Logs(Encoding::Protobuf), &req.encode_to_vec())
    };
    let (one, many) = (need(1), need(1001));
    let resource = size::resource(&big());
    // 1,000 more records cost far less than 1,000 more copies would.
    assert!(many - one < 100 * resource, "{one} for 1 record, {many} for 1,001; resource {resource}");
}

// The pre-scan's estimate is an upper bound on what `prost` allocates, and
// close to it, for every signal.
#[test]
fn protobuf_estimate_covers_the_decoded_size() {
    let u = Limits::UNBOUNDED;
    for n in [1, 5, 64, 300] {
        let b = body(Format::Logs(Encoding::Protobuf), n);
        let (est, real) = (scan::logs(&b, u).unwrap(), size::logs(&ExportLogsServiceRequest::decode(&b[..]).unwrap()));
        assert_eq!(est.records, n);
        assert!(est.bytes >= real && est.bytes <= 2 * real + 4096, "logs {n}: {} for {real}", est.bytes);
        let b = body(Format::Traces(Encoding::Protobuf), n);
        let (est, real) =
            (scan::traces(&b, u).unwrap(), size::traces(&ExportTraceServiceRequest::decode(&b[..]).unwrap()));
        assert_eq!(est.records, n);
        assert!(est.bytes >= real && est.bytes <= 2 * real + 4096, "traces {n}: {} for {real}", est.bytes);
        let b = body(Format::Metrics(Encoding::Protobuf), n);
        let (est, real) =
            (scan::metrics(&b, u).unwrap(), size::metrics(&ExportMetricsServiceRequest::decode(&b[..]).unwrap()));
        assert_eq!(est.records, n);
        assert!(est.bytes >= real && est.bytes <= 2 * real + 4096, "metrics {n}: {} for {real}", est.bytes);
    }
}

// The pre-scan follows `prost`: fields it does not know (scalars, nested
// messages, groups) are skipped, and truncated input is an error.
#[test]
fn scan_accepts_what_prost_accepts() {
    let mut b = body(Format::Logs(Encoding::Protobuf), 3);
    // Field 99: a varint, a length-delimited field and a group, all unknown.
    b.extend_from_slice(&[0x98, 0x06, 0x01]);
    b.extend_from_slice(&[0x9a, 0x06, 0x02, 0xaa, 0xbb]);
    b.extend_from_slice(&[0x9b, 0x06, 0x08, 0x01, 0x9c, 0x06]);
    assert!(ExportLogsServiceRequest::decode(&b[..]).is_ok());
    assert_eq!(scan::logs(&b, Limits::UNBOUNDED).unwrap().records, 3);
    assert_eq!(decode(Format::Logs(Encoding::Protobuf), &b, Limits::UNBOUNDED).unwrap().len(), 3);
    let truncated = &b[..b.len() - 3];
    assert!(ExportLogsServiceRequest::decode(truncated).is_err());
    assert!(matches!(scan::logs(truncated, Limits::UNBOUNDED), Err(IngestError::Malformed(_))));
}
