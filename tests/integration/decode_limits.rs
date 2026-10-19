//! REL-16: decoding limits over HTTP (TC-01). Every ingest format and
//! encoding, plain and gzip, at the event limit (limit − 1, limit,
//! limit + 1), against the decoded-bytes limit and the nesting limit: a
//! structured error naming the limit, nothing of a refused batch stored,
//! the admission slot released and the next request accepted.
//!
//! The decoders' own tests (`crates/ingest/tests/limits.rs`) hold each
//! limit to the exact byte; here the decoded-bytes limit is shown with
//! bodies well on either side of it.

use ingest::otlp::proto::*;
use prost::Message;
use serde_json::{Value, json};
use server::config::{ByteSize, INGEST_DECODE_BYTES_PER_REQUEST_BYTE};

use crate::common::*;

#[derive(Clone, Copy, Debug)]
enum Format {
    NativeArray,
    NativeEnvelope,
    NdJson,
    Clef,
    Otlp(Signal, bool),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Signal {
    Logs,
    Traces,
    Metrics,
}

impl Signal {
    fn name(self) -> &'static str {
        match self {
            Signal::Logs => "logs",
            Signal::Traces => "traces",
            Signal::Metrics => "metrics",
        }
    }
}

const FORMATS: [Format; 10] = [
    Format::NativeArray,
    Format::NativeEnvelope,
    Format::NdJson,
    Format::Clef,
    Format::Otlp(Signal::Logs, false),
    Format::Otlp(Signal::Logs, true),
    Format::Otlp(Signal::Traces, false),
    Format::Otlp(Signal::Traces, true),
    Format::Otlp(Signal::Metrics, false),
    Format::Otlp(Signal::Metrics, true),
];

impl Format {
    fn signal(self) -> Signal {
        match self {
            Format::Otlp(s, _) => s,
            _ => Signal::Logs,
        }
    }

    fn path(self) -> String {
        match self {
            Format::Otlp(s, _) => format!("/v1/{}", s.name()),
            _ => "/api/v1/events".into(),
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Format::NativeArray | Format::NativeEnvelope | Format::Otlp(_, true) => "application/json",
            Format::NdJson => "application/x-ndjson",
            Format::Clef => "application/vnd.serilog.clef",
            Format::Otlp(_, false) => "application/x-protobuf",
        }
    }
}

fn s(v: impl Into<String>) -> Option<AnyValue> {
    Some(AnyValue { value: Some(any_value::Value::StringValue(v.into())) })
}

/// An attribute value: `width` empty values in an array, `depth` arrays
/// deep.
fn list(width: usize, depth: usize) -> Option<AnyValue> {
    let leaf =
        AnyValue { value: Some(any_value::Value::ArrayValue(ArrayValue { values: vec![AnyValue::default(); width] })) };
    let nested = (1..depth).fold(leaf, |inner, _| AnyValue {
        value: Some(any_value::Value::ArrayValue(ArrayValue { values: vec![inner] })),
    });
    Some(nested)
}

fn list_json(width: usize, depth: usize) -> String {
    let leaf = format!(r#"{{"arrayValue":{{"values":[{}]}}}}"#, vec!["{}"; width].join(","));
    (1..depth).fold(leaf, |inner, _| format!(r#"{{"arrayValue":{{"values":[{inner}]}}}}"#))
}

fn resource() -> Option<Resource> {
    Some(Resource { attributes: vec![KeyValue { key: "service.name".into(), value: s("api") }] })
}

/// `n` OTLP records of `signal`, each with one attribute `value` (or none).
fn otlp(signal: Signal, json: bool, n: usize, value: Option<(Option<AnyValue>, String)>) -> Vec<u8> {
    let attrs = || value.iter().map(|(v, _)| KeyValue { key: "a".into(), value: v.clone() }).collect::<Vec<_>>();
    let attrs_json = || match &value {
        Some((_, j)) => format!(r#"[{{"key":"a","value":{j}}}]"#),
        None => "[]".into(),
    };
    let records: Vec<String> = (0..n)
        .map(|i| match signal {
            Signal::Logs => format!(r#"{{"timeUnixNano":"{}","body":{{"stringValue":"log {i}"}},"attributes":{}}}"#, 1000 + i, attrs_json()),
            Signal::Traces => format!(
                r#"{{"traceId":"{:032x}","spanId":"{:016x}","name":"span {i}","startTimeUnixNano":"1","endTimeUnixNano":"2","attributes":{}}}"#,
                i + 1,
                i + 1,
                attrs_json()
            ),
            Signal::Metrics => format!(
                r#"{{"name":"m{i}","gauge":{{"dataPoints":[{{"timeUnixNano":"5","asDouble":1.5,"attributes":{}}}]}}}}"#,
                attrs_json()
            ),
        })
        .collect();
    if json {
        let (outer, scope, items) = match signal {
            Signal::Logs => ("resourceLogs", "scopeLogs", "logRecords"),
            Signal::Traces => ("resourceSpans", "scopeSpans", "spans"),
            Signal::Metrics => ("resourceMetrics", "scopeMetrics", "metrics"),
        };
        return format!(
            r#"{{"{outer}":[{{"resource":{{"attributes":[{{"key":"service.name","value":{{"stringValue":"api"}}}}]}},"{scope}":[{{"{items}":[{}]}}]}}]}}"#,
            records.join(",")
        )
        .into_bytes();
    }
    match signal {
        Signal::Logs => ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: resource(),
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: (0..n)
                        .map(|i| LogRecord {
                            time_unix_nano: 1000 + i as u64,
                            body: s(format!("log {i}")),
                            attributes: attrs(),
                            ..Default::default()
                        })
                        .collect(),
                }],
            }],
        }
        .encode_to_vec(),
        Signal::Traces => ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: resource(),
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: (0..n)
                        .map(|i| Span {
                            trace_id: (i as u128 + 1).to_be_bytes().to_vec(),
                            span_id: (i as u64 + 1).to_be_bytes().to_vec(),
                            name: format!("span {i}"),
                            start_time_unix_nano: 1,
                            end_time_unix_nano: 2,
                            attributes: attrs(),
                            ..Default::default()
                        })
                        .collect(),
                }],
            }],
        }
        .encode_to_vec(),
        Signal::Metrics => ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: resource(),
                scope_metrics: vec![ScopeMetrics {
                    scope: None,
                    metrics: (0..n)
                        .map(|i| Metric {
                            name: format!("m{i}"),
                            data: Some(metric::Data::Gauge(Gauge {
                                data_points: vec![NumberDataPoint {
                                    time_unix_nano: 5,
                                    attributes: attrs(),
                                    value: Some(number_data_point::Value::AsDouble(1.5)),
                                    ..Default::default()
                                }],
                            })),
                            ..Default::default()
                        })
                        .collect(),
                }],
            }],
        }
        .encode_to_vec(),
    }
}

/// `n` native events, each `event(i)`.
fn native(f: Format, n: usize, event: impl Fn(usize) -> String) -> Vec<u8> {
    let events: Vec<String> = (0..n).map(event).collect();
    match f {
        Format::NativeArray => format!("[{}]", events.join(",")),
        Format::NativeEnvelope => format!(r#"{{"events":[{}]}}"#, events.join(",")),
        _ => events.join("\n"),
    }
    .into_bytes()
}

/// `n` ordinary records.
fn ordinary(f: Format, n: usize) -> Vec<u8> {
    match f {
        Format::Clef => native(f, n, |i| format!(r#"{{"@t":"2026-10-01T00:00:00Z","@mt":"Order {{Id}}","Id":{i}}}"#)),
        Format::Otlp(sig, json) => otlp(sig, json, n, None),
        _ => native(f, n, |i| format!(r#"{{"message":"event {i}","properties":{{"customerId":{i}}}}}"#)),
    }
}

/// About `size` bytes that decode into far more than the 17× allowance:
/// tiny native events, or OTLP records each holding a few hundred empty
/// attribute values.
fn amplifying(f: Format, size: usize) -> Vec<u8> {
    let fit = |make: &dyn Fn(usize) -> Vec<u8>| {
        let per = make(10).len() / 10;
        let mut n = size / per;
        while make(n).len() > size {
            n -= n / 20 + 1;
        }
        make(n)
    };
    match f {
        Format::Otlp(sig, json) => fit(&|n| otlp(sig, json, n, Some((list(300, 1), list_json(300, 1))))),
        _ => fit(&|n| native(f, n, |_| "{}".into())),
    }
}

/// A body whose single record holds a value `depth` levels deep.
fn nested(f: Format, depth: usize) -> Vec<u8> {
    match f {
        Format::Otlp(sig, json) => otlp(sig, json, 1, Some((list(1, depth), list_json(1, depth)))),
        // The event is at depth 0, so the property's arrays start at 1.
        _ => native(f, 1, |_| format!(r#"{{"message":"deep","p":{}0{}}}"#, "[".repeat(depth), "]".repeat(depth))),
    }
}

async fn send(s: &TestServer, f: Format, gzip: bool, body: Vec<u8>) -> (u16, Value) {
    let mut req = s.client.post(s.u(&f.path())).header("content-type", f.content_type());
    let body = if gzip {
        req = req.header("content-encoding", "gzip");
        crate::common::gzip(&body)
    } else {
        body
    };
    let r = req.body(body).send().await.unwrap();
    let status = r.status().as_u16();
    let bytes = r.bytes().await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn count(s: &TestServer, signal: Signal) -> u64 {
    s.post_ok(&format!("/api/v1/query/{}/count", signal.name()), json!({ "query": "" })).await["count"]
        .as_u64()
        .unwrap()
}

async fn counts(s: &TestServer) -> [u64; 3] {
    [count(s, Signal::Logs).await, count(s, Signal::Traces).await, count(s, Signal::Metrics).await]
}

fn add(c: &mut [u64; 3], signal: Signal, n: u64) {
    c[signal as usize] += n;
}

// TC-01 / AC2: `ingest.queue_capacity` events per request, exactly, for
// every format and encoding; one more is a 413 naming the limit.
#[tokio::test]
async fn event_limit_for_every_format_and_encoding() {
    const LIMIT: usize = 4;
    let s = TestServer::start_with(|c| c.ingest.queue_capacity = LIMIT).await;
    let mut expected = [0u64; 3];
    for f in FORMATS {
        for gzip in [false, true] {
            for n in [LIMIT - 1, LIMIT, LIMIT + 1] {
                let what = format!("{f:?} gzip={gzip} n={n}");
                let (status, body) = send(&s, f, gzip, ordinary(f, n)).await;
                if n <= LIMIT {
                    assert_eq!(status, 200, "{what}: {body}");
                    add(&mut expected, f.signal(), n as u64);
                } else {
                    assert_eq!(status, 413, "{what}");
                    let e = &body["error"];
                    assert_eq!(e["code"], "payload_too_large", "{what}");
                    assert_eq!(e["limit"], "maxEvents", "{what}");
                    assert_eq!(e["maxEvents"], LIMIT, "{what}");
                    let message = e["message"].as_str().unwrap();
                    assert!(message.contains("ingest.queue_capacity") && message.contains("split"), "{message}");
                }
                wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
            }
        }
    }
    // Every accepted batch was stored whole, nothing of a refused one.
    assert_eq!(counts(&s).await, expected);
    s.stop().await;
}

// TC-01 / AC1: bodies within `max_request_size` that would decode past the
// allowance are refused with 413 `maxDecodedBytes`; ordinary bodies of the
// same size are accepted.
#[tokio::test]
async fn decoded_bytes_limit_for_every_format_and_encoding() {
    const MAX_REQUEST: usize = 64 << 10;
    let s = TestServer::start_with(|c| {
        c.ingest.max_request_size = ByteSize(MAX_REQUEST as u64);
        c.ingest.queue_capacity = 1_000_000;
    })
    .await;
    let allowance = MAX_REQUEST as u64 * INGEST_DECODE_BYTES_PER_REQUEST_BYTE;
    assert_eq!(s.config.ingest_limits().max_decoded_bytes as u64, allowance);
    let mut expected = [0u64; 3];
    for f in FORMATS {
        for gzip in [false, true] {
            let what = format!("{f:?} gzip={gzip}");
            let bad = amplifying(f, MAX_REQUEST);
            assert!(bad.len() <= MAX_REQUEST, "{what}: {} bytes", bad.len());
            let (status, body) = send(&s, f, gzip, bad).await;
            assert_eq!(status, 413, "{what}: {body}");
            let e = &body["error"];
            assert_eq!(e["code"], "payload_too_large", "{what}");
            assert_eq!(e["limit"], "maxDecodedBytes", "{what}");
            assert_eq!(e["limitBytes"], allowance, "{what}");
            assert!(e["message"].as_str().unwrap().contains("Split it into smaller requests"), "{what}: {e}");
            wait_until("slot freed", || ingest_in_flight(&s) == 0).await;

            let good = ordinary(f, 200);
            assert!(good.len() <= MAX_REQUEST);
            let (status, body) = send(&s, f, gzip, good).await;
            assert_eq!(status, 200, "{what}: {body}");
            add(&mut expected, f.signal(), 200);
        }
    }
    assert_eq!(counts(&s).await, expected, "refused batches stored nothing");
    s.stop().await;
}

// TC-01: values nested past the limit are a 400 naming it.
#[tokio::test]
async fn nesting_limit_for_every_format_and_encoding() {
    let s = TestServer::start().await;
    let limit = ingest::MAX_DEPTH;
    let mut expected = [0u64; 3];
    for f in FORMATS {
        for gzip in [false, true] {
            let what = format!("{f:?} gzip={gzip}");
            let (status, body) = send(&s, f, gzip, nested(f, limit)).await;
            assert_eq!(status, 200, "{what} at the limit: {body}");
            add(&mut expected, f.signal(), 1);
            let (status, body) = send(&s, f, gzip, nested(f, limit + 1)).await;
            assert_eq!(status, 400, "{what} past the limit: {body}");
            let e = &body["error"];
            assert_eq!(e["code"], "invalid_payload", "{what}");
            assert_eq!((e["limit"].as_str(), e["maxDepth"].as_u64()), (Some("maxDepth"), Some(limit as u64)), "{what}");
            wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
        }
    }
    assert_eq!(counts(&s).await, expected);
    s.stop().await;
}

/// `n` log records the way an OpenTelemetry SDK exports them: an SDK
/// resource, a message, a couple of attributes and trace context (about
/// 100 bytes each, so 8,192 fit the default 1 MB request).
fn sdk_logs(n: usize) -> Vec<u8> {
    let kv = |k: &str, v: Option<AnyValue>| KeyValue { key: k.into(), value: v };
    let int = |i: i64| Some(AnyValue { value: Some(any_value::Value::IntValue(i)) });
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![
                    kv("service.name", s("checkout")),
                    kv("service.version", s("1.4.2")),
                    kv("deployment.environment.name", s("production")),
                    kv("host.name", s("ip-10-0-1-17")),
                    kv("telemetry.sdk.name", s("opentelemetry")),
                    kv("telemetry.sdk.language", s("dotnet")),
                    kv("telemetry.sdk.version", s("1.9.0")),
                ],
            }),
            scope_logs: vec![ScopeLogs {
                scope: Some(InstrumentationScope { name: "Checkout.Api".into(), ..Default::default() }),
                log_records: (0..n)
                    .map(|i| LogRecord {
                        time_unix_nano: 1_760_000_000_000_000_000 + i as u64,
                        severity_number: 9,
                        body: s(format!("Handled /api/orders in {} ms", i % 900)),
                        attributes: vec![kv("http.route", s("/api/orders")), kv("http.status", int(200))],
                        trace_id: (i as u128 + 1).to_be_bytes().to_vec(),
                        span_id: (i as u64 + 1).to_be_bytes().to_vec(),
                        flags: 1,
                        ..Default::default()
                    })
                    .collect(),
            }],
        }],
    }
    .encode_to_vec()
}

// AC4: default export batches of the OpenTelemetry SDKs (512 records) and
// the Collector's batch processor (8,192) are accepted unchanged with the
// default configuration.
#[tokio::test]
async fn sdk_and_collector_default_batches_are_accepted() {
    let s = TestServer::start().await;
    let f = Format::Otlp(Signal::Logs, false);
    let mut stored = 0;
    for n in [512, 8192] {
        let body = sdk_logs(n);
        assert!(body.len() <= s.config.ingest.max_request_size.0 as usize, "{n} records: {} bytes", body.len());
        for gzip in [false, true] {
            let (status, body) = send(&s, f, gzip, body.clone()).await;
            assert_eq!(status, 200, "{n} records, gzip={gzip}: {body}");
            stored += n as u64;
        }
    }
    assert_eq!(count(&s, Signal::Logs).await, stored);
    s.stop().await;
}
