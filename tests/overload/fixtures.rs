//! Deterministic ingest fixtures for the amplification measurements
//! (REL-14). Every fixture is generated from a fixed seed, so a rerun
//! builds byte-identical bodies; sizes and event counts are printed with
//! the results.
//!
//! OTLP fixtures are built once as protobuf structs and encoded both as
//! protobuf and as OTLP/JSON, so the two encodings carry the same records.
//!
//! Each fixture says what the server does with it at the request-size
//! limit ([`Expect`]): representative producer batches are accepted, and
//! shapes that would decode past the per-request estimate are refused
//! (REL-16). Either way the request stays within the estimate.

use ingest::otlp::proto::*;
use serde_json::{Value as Json, json};

/// Seed for every generated fixture.
pub const SEED: u64 = 0x5EED_0014;

/// xorshift64*: small, deterministic, good enough for filler.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }

    pub fn word(&mut self, len: usize) -> String {
        (0..len).map(|_| (b'a' + self.below(26) as u8) as char).collect()
    }

    pub fn hex(&mut self, bytes: usize) -> String {
        (0..bytes).map(|_| format!("{:02x}", self.below(256))).collect()
    }

    pub fn id(&mut self, bytes: usize) -> Vec<u8> {
        (0..bytes).map(|_| self.below(255) as u8 + 1).collect()
    }
}

/// Where a fixture is sent and how it is decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    NativeJson,
    NativeNdJson,
    OtlpLogs(ingest::otlp::Encoding),
    OtlpTraces(ingest::otlp::Encoding),
    OtlpMetrics(ingest::otlp::Encoding),
}

impl Kind {
    pub fn path(self) -> &'static str {
        match self {
            Kind::NativeJson | Kind::NativeNdJson => "/api/v1/events",
            Kind::OtlpLogs(_) => "/v1/logs",
            Kind::OtlpTraces(_) => "/v1/traces",
            Kind::OtlpMetrics(_) => "/v1/metrics",
        }
    }

    pub fn content_type(self) -> &'static str {
        use ingest::otlp::Encoding::*;
        match self {
            Kind::NativeJson => "application/json",
            Kind::NativeNdJson => "application/x-ndjson",
            Kind::OtlpLogs(Json) | Kind::OtlpTraces(Json) | Kind::OtlpMetrics(Json) => "application/json",
            _ => "application/x-protobuf",
        }
    }

    /// Run the same decoding the server runs, within `limits`.
    pub fn parse(
        self,
        body: &[u8],
        limits: ingest::Limits,
    ) -> Result<Vec<telemetry::TelemetryEvent>, ingest::IngestError> {
        use ingest::{native, otlp};
        let now = telemetry::Timestamp::now();
        match self {
            Kind::NativeJson => native::parse(body, native::Format::Json, now, limits),
            Kind::NativeNdJson => native::parse(body, native::Format::NdJson, now, limits),
            Kind::OtlpLogs(e) => otlp::decode_logs(body, e, now, limits).map(|m| m.events),
            Kind::OtlpTraces(e) => otlp::decode_traces(body, e, now, limits).map(|m| m.events),
            Kind::OtlpMetrics(e) => otlp::decode_metrics(body, e, now, limits).map(|m| m.events),
        }
    }
}

/// What the server does with a fixture sent at the request-size limit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expect {
    /// Stored: representative producer batches, and shapes that decode
    /// within the per-request estimate.
    Accepted,
    /// Refused with 413 (`maxDecodedBytes`) while decoding, before it
    /// passes the estimate: shapes that cost far more memory decoded than
    /// on the wire.
    Refused,
}

pub struct Fixture {
    pub name: String,
    pub kind: Kind,
    /// What one record looks like, for the results table.
    pub shape: &'static str,
    /// Decoded (uncompressed) body.
    pub body: Vec<u8>,
    pub events: usize,
    pub expect: Expect,
}

/// Build `make(n)` with `n` as large as possible while the body stays at
/// or under `target` bytes. `make` must grow monotonically with `n`.
/// Doubles from a probe-based guess, then bisects, so fixtures whose fixed
/// part (such as a large shared resource) dominates a small probe are still
/// filled to the target.
fn fit(target: usize, make: impl Fn(usize) -> Vec<u8>) -> (Vec<u8>, usize) {
    let probe = 64;
    let per = make(probe).len().div_ceil(probe).max(1);
    let (mut lo, mut hi) = (1, (target / per).max(2));
    while make(hi).len() <= target {
        lo = hi;
        hi *= 2;
    }
    // make(lo) fits (or lo is 1), make(hi) does not.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if make(mid).len() <= target { lo = mid } else { hi = mid }
    }
    (make(lo), lo)
}

// ---- native -------------------------------------------------------------

const SERVICES: [&str; 6] = ["checkout", "payments", "inventory", "search", "auth", "gateway"];
const LEVELS: [&str; 5] = ["Debug", "Information", "Information", "Warning", "Error"];
const STATUS: [u16; 4] = [200, 201, 404, 500];
const PATHS: [&str; 5] = ["/api/orders", "/api/cart", "/api/users/me", "/health", "/api/search"];

/// A typical structured application event, ~330 bytes as JSON.
fn representative_event(r: &mut Rng, i: usize) -> Json {
    json!({
        "timestamp": format!("2026-10-11T10:{:02}:{:02}.{:03}Z", i / 60 % 60, i % 60, i % 1000),
        "level": r.pick(&LEVELS),
        "service": r.pick(&SERVICES),
        "environment": "production",
        "message": format!("Handled {} in {} ms", r.pick(&PATHS), r.below(900)),
        "traceId": r.hex(16),
        "spanId": r.hex(8),
        "properties": {
            "customerId": r.below(100_000),
            "requestPath": r.pick(&PATHS),
            "statusCode": STATUS[r.below(4) as usize],
            "elapsedMs": r.below(90_000) as f64 / 100.0,
            "region": r.pick(&["us-east-1", "eu-west-1", "ap-southeast-2"]),
        }
    })
}

/// Serilog CLEF, as written by `Serilog.Formatting.Compact`.
fn clef_event(r: &mut Rng, i: usize) -> Json {
    let mut e = json!({
        "@t": format!("2026-10-11T10:{:02}:{:02}.{:03}Z", i / 60 % 60, i % 60, i % 1000),
        "@mt": "HTTP {RequestMethod} {RequestPath} responded {StatusCode} in {Elapsed:0.0000} ms",
        "RequestMethod": r.pick(&["GET", "POST", "PUT"]),
        "RequestPath": r.pick(&PATHS),
        "StatusCode": STATUS[r.below(4) as usize],
        "Elapsed": r.below(90_000) as f64 / 100.0,
        "SourceContext": "Serilog.AspNetCore.RequestLoggingMiddleware",
        "RequestId": r.hex(8),
        "ConnectionId": r.word(13).to_uppercase(),
    });
    if i.is_multiple_of(10) {
        e["@l"] = json!("Error");
        e["@x"] = json!(format!(
            "System.InvalidOperationException: Sequence contains no elements\n   at System.Linq.ThrowHelper.ThrowNoElementsException()\n   at Shop.Orders.Handle(Int32 id) in /src/Orders.cs:line {}",
            r.below(400)
        ));
    }
    e
}

/// Many small objects and arrays: each value is its own allocation.
fn nested_event(r: &mut Rng, _: usize) -> Json {
    let leaf = |r: &mut Rng| json!({ "k": r.below(10), "v": [r.below(9), r.below(9), r.below(9)], "t": true });
    json!({
        "message": "nested",
        "properties": {
            "a": { "b": { "c": { "d": leaf(r), "e": leaf(r) }, "f": [leaf(r), leaf(r)] } },
            "g": [[1, 2], [3, 4], [5, [6, [7, [8]]]]],
            "h": { "i": null, "j": 1.5, "k": "x", "l": {}, "m": [] },
        }
    })
}

/// Few events, each carrying long strings: close to 1:1 in memory.
fn string_heavy_event(r: &mut Rng, _: usize) -> Json {
    json!({
        "message": r.word(2000),
        "properties": { "payload": r.word(6000), "stack": r.word(4000) }
    })
}

fn ndjson(events: impl Iterator<Item = Json>) -> Vec<u8> {
    let mut out = Vec::new();
    for e in events {
        serde_json::to_writer(&mut out, &e).unwrap();
        out.push(b'\n');
    }
    out
}

fn native(
    name: &str,
    kind: Kind,
    shape: &'static str,
    target: usize,
    event: fn(&mut Rng, usize) -> Json,
    expect: Expect,
) -> Fixture {
    let (body, events) = fit(target, |n| {
        let mut r = Rng::new(SEED);
        let items = (0..n).map(|i| event(&mut r, i));
        match kind {
            Kind::NativeJson => serde_json::to_vec(&items.collect::<Vec<_>>()).unwrap(),
            _ => ndjson(items),
        }
    });
    Fixture { name: name.into(), kind, shape, body, events, expect }
}

// ---- OTLP ---------------------------------------------------------------

fn s(v: impl Into<String>) -> Option<AnyValue> {
    Some(AnyValue { value: Some(any_value::Value::StringValue(v.into())) })
}

fn int(v: i64) -> Option<AnyValue> {
    Some(AnyValue { value: Some(any_value::Value::IntValue(v)) })
}

fn kv(k: &str, value: Option<AnyValue>) -> KeyValue {
    KeyValue { key: k.into(), value }
}

/// A resource as the OpenTelemetry SDKs report it.
fn sdk_resource(r: &mut Rng) -> Resource {
    Resource {
        attributes: vec![
            kv("service.name", s(r.pick(&SERVICES))),
            kv("service.version", s("1.4.2")),
            kv("deployment.environment.name", s("production")),
            kv("host.name", s(format!("ip-10-0-{}-{}", r.below(255), r.below(255)))),
            kv("telemetry.sdk.name", s("opentelemetry")),
            kv("telemetry.sdk.language", s("dotnet")),
            kv("telemetry.sdk.version", s("1.9.0")),
        ],
    }
}

fn scope() -> Option<InstrumentationScope> {
    Some(InstrumentationScope { name: "Shop.Api".into(), version: "1.0.0".into(), attributes: vec![] })
}

fn log_record(r: &mut Rng, i: usize) -> LogRecord {
    LogRecord {
        time_unix_nano: 1_760_000_000_000_000_000 + i as u64 * 1_000_000,
        observed_time_unix_nano: 1_760_000_000_000_000_000 + i as u64 * 1_000_000,
        severity_number: [5, 9, 9, 13, 17][r.below(5) as usize],
        severity_text: String::new(),
        body: s(format!("Handled {} in {} ms", r.pick(&PATHS), r.below(900))),
        attributes: vec![
            kv("http.route", s(r.pick(&PATHS))),
            kv("http.response.status_code", int([200, 201, 404, 500][r.below(4) as usize])),
            kv("customer.id", int(r.below(100_000) as i64)),
            kv("{OriginalFormat}", s("Handled {Route} in {Elapsed} ms")),
        ],
        flags: 1,
        trace_id: r.id(16),
        span_id: r.id(8),
        event_name: String::new(),
    }
}

fn logs_request(resources: Vec<(Resource, Vec<LogRecord>)>) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest {
        resource_logs: resources
            .into_iter()
            .map(|(res, log_records)| ResourceLogs {
                resource: Some(res),
                scope_logs: vec![ScopeLogs { scope: scope(), log_records }],
            })
            .collect(),
    }
}

fn span(r: &mut Rng, i: usize) -> Span {
    let start = 1_760_000_000_000_000_000 + i as u64 * 1_000_000;
    Span {
        trace_id: r.id(16),
        span_id: r.id(8),
        trace_state: String::new(),
        parent_span_id: if i.is_multiple_of(4) { vec![] } else { r.id(8) },
        name: format!("{} {}", r.pick(&["GET", "POST"]), r.pick(&PATHS)),
        kind: 2,
        start_time_unix_nano: start,
        end_time_unix_nano: start + r.below(500_000_000),
        attributes: vec![
            kv("http.request.method", s("GET")),
            kv("http.route", s(r.pick(&PATHS))),
            kv("http.response.status_code", int(200)),
            kv("server.address", s("shop.internal")),
            kv("db.system", s("postgresql")),
        ],
        events: if i.is_multiple_of(8) {
            vec![SpanEvent {
                time_unix_nano: start + 10,
                name: "exception".into(),
                attributes: vec![kv("exception.type", s("TimeoutException"))],
            }]
        } else {
            vec![]
        },
        links: vec![],
        status: Some(Status { message: String::new(), code: if i.is_multiple_of(10) { 2 } else { 0 } }),
    }
}

fn traces_request(res: Resource, spans: Vec<Span>) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(res),
            scope_spans: vec![ScopeSpans { scope: scope(), spans }],
        }],
    }
}

/// A metric with one data point per series, cycling through gauge, sum
/// and histogram.
fn metric(r: &mut Rng, i: usize) -> Metric {
    use metric::Data;
    let attrs = |r: &mut Rng| {
        vec![kv("http.route", s(r.pick(&PATHS))), kv("http.response.status_code", int(200)), kv("host", s(r.word(8)))]
    };
    let t = 1_760_000_000_000_000_000 + i as u64 * 1_000_000_000;
    let number = |r: &mut Rng| NumberDataPoint {
        attributes: attrs(r),
        start_time_unix_nano: t - 60_000_000_000,
        time_unix_nano: t,
        value: Some(number_data_point::Value::AsDouble(r.below(10_000) as f64 / 10.0)),
    };
    let data = match i % 3 {
        0 => Data::Gauge(Gauge { data_points: vec![number(r), number(r)] }),
        1 => Data::Sum(Sum { data_points: vec![number(r), number(r)], aggregation_temporality: 2, is_monotonic: true }),
        _ => Data::Histogram(Histogram {
            data_points: vec![HistogramDataPoint {
                attributes: attrs(r),
                start_time_unix_nano: t - 60_000_000_000,
                time_unix_nano: t,
                count: 100,
                sum: Some(1234.5),
                bucket_counts: (0..11).map(|_| r.below(20)).collect(),
                explicit_bounds: vec![5.0, 10.0, 25.0, 50.0, 75.0, 100.0, 250.0, 500.0, 750.0, 1000.0],
                min: Some(1.0),
                max: Some(990.0),
            }],
            aggregation_temporality: 2,
        }),
    };
    Metric {
        name: format!("http.server.{}", ["active_requests", "requests", "request.duration"][i % 3]),
        description: String::new(),
        unit: ["{request}", "{request}", "ms"][i % 3].into(),
        data: Some(data),
    }
}

fn metrics_request(res: Resource, metrics: Vec<Metric>) -> ExportMetricsServiceRequest {
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(res),
            scope_metrics: vec![ScopeMetrics { scope: scope(), metrics }],
        }],
    }
}

// ---- OTLP/JSON encoding (the subset the generators above produce) -------

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn any_json(v: &Option<AnyValue>) -> Json {
    use any_value::Value as V;
    match v.as_ref().and_then(|v| v.value.as_ref()) {
        Some(V::StringValue(s)) => json!({ "stringValue": s }),
        Some(V::IntValue(n)) => json!({ "intValue": n.to_string() }),
        Some(V::DoubleValue(d)) => json!({ "doubleValue": d }),
        Some(V::BoolValue(b)) => json!({ "boolValue": b }),
        Some(V::ArrayValue(a)) => {
            json!({ "arrayValue": { "values": a.values.iter().map(|v| any_json(&Some(v.clone()))).collect::<Vec<_>>() } })
        }
        Some(V::KvlistValue(l)) => json!({ "kvlistValue": { "values": kvs_json(&l.values) } }),
        Some(V::BytesValue(_)) | None => json!({}),
    }
}

fn kvs_json(kvs: &[KeyValue]) -> Vec<Json> {
    kvs.iter().map(|kv| json!({ "key": kv.key, "value": any_json(&kv.value) })).collect()
}

fn resource_json(r: &Option<Resource>) -> Json {
    json!({ "attributes": kvs_json(&r.as_ref().map(|r| r.attributes.clone()).unwrap_or_default()) })
}

fn scope_json(s: &Option<InstrumentationScope>) -> Json {
    s.as_ref().map_or(json!({}), |s| json!({ "name": s.name, "version": s.version }))
}

fn logs_json(req: &ExportLogsServiceRequest) -> Vec<u8> {
    let rl: Vec<Json> = req
        .resource_logs
        .iter()
        .map(|rl| {
            json!({
                "resource": resource_json(&rl.resource),
                "scopeLogs": rl.scope_logs.iter().map(|sl| json!({
                    "scope": scope_json(&sl.scope),
                    "logRecords": sl.log_records.iter().map(|lr| {
                        let mut o = json!({
                            "timeUnixNano": lr.time_unix_nano.to_string(),
                            "observedTimeUnixNano": lr.observed_time_unix_nano.to_string(),
                            "severityNumber": lr.severity_number,
                            "attributes": kvs_json(&lr.attributes),
                            "flags": lr.flags,
                            "traceId": hex(&lr.trace_id),
                            "spanId": hex(&lr.span_id),
                        });
                        if lr.body.is_some() {
                            o["body"] = any_json(&lr.body);
                        }
                        o
                    }).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "resourceLogs": rl })).unwrap()
}

fn traces_json(req: &ExportTraceServiceRequest) -> Vec<u8> {
    let rs: Vec<Json> = req
        .resource_spans
        .iter()
        .map(|rs| {
            json!({
                "resource": resource_json(&rs.resource),
                "scopeSpans": rs.scope_spans.iter().map(|ss| json!({
                    "scope": scope_json(&ss.scope),
                    "spans": ss.spans.iter().map(|sp| json!({
                        "traceId": hex(&sp.trace_id),
                        "spanId": hex(&sp.span_id),
                        "parentSpanId": hex(&sp.parent_span_id),
                        "name": sp.name,
                        "kind": sp.kind,
                        "startTimeUnixNano": sp.start_time_unix_nano.to_string(),
                        "endTimeUnixNano": sp.end_time_unix_nano.to_string(),
                        "attributes": kvs_json(&sp.attributes),
                        "events": sp.events.iter().map(|e| json!({
                            "timeUnixNano": e.time_unix_nano.to_string(),
                            "name": e.name,
                            "attributes": kvs_json(&e.attributes),
                        })).collect::<Vec<_>>(),
                        "status": sp.status.as_ref().map_or(json!({}), |st| json!({ "code": st.code, "message": st.message })),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "resourceSpans": rs })).unwrap()
}

fn number_json(p: &NumberDataPoint) -> Json {
    let mut o = json!({
        "attributes": kvs_json(&p.attributes),
        "startTimeUnixNano": p.start_time_unix_nano.to_string(),
        "timeUnixNano": p.time_unix_nano.to_string(),
    });
    match p.value {
        Some(number_data_point::Value::AsDouble(d)) => o["asDouble"] = json!(d),
        Some(number_data_point::Value::AsInt(n)) => o["asInt"] = json!(n.to_string()),
        None => {}
    }
    o
}

fn metrics_json(req: &ExportMetricsServiceRequest) -> Vec<u8> {
    use metric::Data;
    let rm: Vec<Json> = req
        .resource_metrics
        .iter()
        .map(|rm| {
            json!({
                "resource": resource_json(&rm.resource),
                "scopeMetrics": rm.scope_metrics.iter().map(|sm| json!({
                    "scope": scope_json(&sm.scope),
                    "metrics": sm.metrics.iter().map(|m| {
                        let mut o = json!({ "name": m.name, "description": m.description, "unit": m.unit });
                        match &m.data {
                            Some(Data::Gauge(g)) => o["gauge"] = json!({ "dataPoints": g.data_points.iter().map(number_json).collect::<Vec<_>>() }),
                            Some(Data::Sum(s)) => o["sum"] = json!({
                                "dataPoints": s.data_points.iter().map(number_json).collect::<Vec<_>>(),
                                "aggregationTemporality": s.aggregation_temporality,
                                "isMonotonic": s.is_monotonic,
                            }),
                            Some(Data::Histogram(h)) => o["histogram"] = json!({
                                "dataPoints": h.data_points.iter().map(|p| json!({
                                    "attributes": kvs_json(&p.attributes),
                                    "startTimeUnixNano": p.start_time_unix_nano.to_string(),
                                    "timeUnixNano": p.time_unix_nano.to_string(),
                                    "count": p.count.to_string(),
                                    "sum": p.sum,
                                    "bucketCounts": p.bucket_counts.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                                    "explicitBounds": p.explicit_bounds,
                                    "min": p.min,
                                    "max": p.max,
                                })).collect::<Vec<_>>(),
                                "aggregationTemporality": h.aggregation_temporality,
                            }),
                            _ => {}
                        }
                        o
                    }).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "resourceMetrics": rm })).unwrap()
}

/// Both encodings of one OTLP shape, each sized to `target`.
#[allow(clippy::too_many_arguments)]
fn otlp<T: prost::Message>(
    name: &str,
    shape: &'static str,
    target: usize,
    // Per encoding: protobuf, JSON.
    expect: [Expect; 2],
    signal: fn(ingest::otlp::Encoding) -> Kind,
    build: impl Fn(usize) -> T,
    to_json: fn(&T) -> Vec<u8>,
    events_of: impl Fn(&T) -> usize,
) -> [Fixture; 2] {
    use ingest::otlp::Encoding;
    let mut out = Vec::new();
    for ((enc, label), expect) in [(Encoding::Protobuf, "protobuf"), (Encoding::Json, "JSON")].into_iter().zip(expect) {
        let (body, n) = fit(target, |n| match enc {
            Encoding::Protobuf => build(n).encode_to_vec(),
            Encoding::Json => to_json(&build(n)),
        });
        let events = events_of(&build(n));
        out.push(Fixture { name: format!("OTLP {label} {name}"), kind: signal(enc), shape, body, events, expect });
    }
    out.try_into().ok().unwrap()
}

/// Every fixture, decoded size at most `target` bytes.
pub fn all(target: usize) -> Vec<Fixture> {
    let mut v = vec![
        native(
            "native JSON representative",
            Kind::NativeJson,
            "app event, 5 properties",
            target,
            representative_event,
            Expect::Accepted,
        ),
        native(
            "NDJSON representative",
            Kind::NativeNdJson,
            "app event, 5 properties",
            target,
            representative_event,
            Expect::Accepted,
        ),
        native(
            "CLEF representative",
            Kind::NativeNdJson,
            "Serilog request log, 10% with @x",
            target,
            clef_event,
            Expect::Accepted,
        ),
        native(
            "NDJSON nested properties",
            Kind::NativeNdJson,
            "6-level nesting, ~40 small values",
            target,
            nested_event,
            Expect::Refused,
        ),
        native(
            "native JSON nested properties",
            Kind::NativeJson,
            "6-level nesting, ~40 small values",
            target,
            nested_event,
            Expect::Refused,
        ),
        native(
            "NDJSON string-heavy",
            Kind::NativeNdJson,
            "12 KB of strings per event",
            target,
            string_heavy_event,
            Expect::Accepted,
        ),
        native(
            "NDJSON many small events",
            Kind::NativeNdJson,
            "{\"message\":\"…\"} (~30 B)",
            target,
            |r, i| json!({ "message": format!("{}{i}", r.word(4)) }),
            Expect::Accepted,
        ),
        native(
            "native JSON many small events",
            Kind::NativeJson,
            "{\"message\":\"…\"} (~30 B)",
            target,
            |r, i| json!({ "message": format!("{}{i}", r.word(4)) }),
            Expect::Accepted,
        ),
        native("NDJSON empty objects", Kind::NativeNdJson, "{} (3 B)", target, |_, _| json!({}), Expect::Refused),
        native(
            "NDJSON wide arrays",
            Kind::NativeNdJson,
            "500 one-digit numbers in one property",
            target,
            |r, _| json!({ "message": "wide", "properties": { "v": (0..500).map(|_| r.below(10)).collect::<Vec<_>>() } }),
            Expect::Refused,
        ),
    ];
    let logs_events = |r: &ExportLogsServiceRequest| {
        r.resource_logs.iter().flat_map(|x| &x.scope_logs).map(|x| x.log_records.len()).sum()
    };
    v.extend(otlp(
        "logs representative",
        "SDK resource, 4 attributes",
        target,
        [Expect::Accepted, Expect::Accepted],
        Kind::OtlpLogs,
        |n| {
            let mut r = Rng::new(SEED);
            let res = sdk_resource(&mut r);
            logs_request(vec![(res, (0..n).map(|i| log_record(&mut r, i)).collect())])
        },
        logs_json,
        logs_events,
    ));
    v.extend(otlp(
        "logs many small records",
        "body only (~20 B)",
        target,
        [Expect::Refused, Expect::Accepted],
        Kind::OtlpLogs,
        |n| {
            let mut r = Rng::new(SEED);
            let res = sdk_resource(&mut r);
            let records = (0..n).map(|i| LogRecord { body: s(format!("{}{i}", r.word(4))), ..Default::default() });
            logs_request(vec![(res, records.collect())])
        },
        logs_json,
        logs_events,
    ));
    v.extend(otlp(
        "logs large shared resource",
        "200-attribute resource, tiny records",
        target,
        [Expect::Refused, Expect::Accepted],
        Kind::OtlpLogs,
        |n| {
            let mut r = Rng::new(SEED);
            let res = Resource { attributes: (0..200).map(|k| kv(&format!("attr.{k}"), s(r.word(24)))).collect() };
            let records = (0..n).map(|i| LogRecord { body: s(format!("{i}")), ..Default::default() });
            logs_request(vec![(res, records.collect())])
        },
        logs_json,
        logs_events,
    ));
    v.extend(otlp(
        "logs large resource, representative records",
        "200-attribute resource shared by SDK-like records",
        target,
        [Expect::Accepted, Expect::Accepted],
        Kind::OtlpLogs,
        |n| {
            let mut r = Rng::new(SEED);
            let res = Resource { attributes: (0..200).map(|k| kv(&format!("attr.{k}"), s(r.word(24)))).collect() };
            logs_request(vec![(res, (0..n).map(|i| log_record(&mut r, i)).collect())])
        },
        logs_json,
        logs_events,
    ));
    v.extend(otlp(
        "logs empty array values",
        "1,000 empty AnyValues in one attribute",
        target,
        [Expect::Refused, Expect::Refused],
        Kind::OtlpLogs,
        |n| {
            let mut r = Rng::new(SEED);
            let res = sdk_resource(&mut r);
            let list = || AnyValue {
                value: Some(any_value::Value::ArrayValue(ArrayValue { values: vec![AnyValue::default(); 1000] })),
            };
            let records =
                (0..n).map(|_| LogRecord { attributes: vec![kv("list", Some(list()))], ..Default::default() });
            logs_request(vec![(res, records.collect())])
        },
        logs_json,
        logs_events,
    ));
    v.extend(otlp(
        "traces representative",
        "SDK resource, 5 attributes, 1 in 8 with an event",
        target,
        [Expect::Accepted, Expect::Accepted],
        Kind::OtlpTraces,
        |n| {
            let mut r = Rng::new(SEED);
            let res = sdk_resource(&mut r);
            traces_request(res, (0..n).map(|i| span(&mut r, i)).collect())
        },
        traces_json,
        |r| r.resource_spans.iter().flat_map(|x| &x.scope_spans).map(|x| x.spans.len()).sum(),
    ));
    v.extend(otlp(
        "metrics representative",
        "gauge/sum (2 points), histogram (11 buckets)",
        target,
        [Expect::Accepted, Expect::Accepted],
        Kind::OtlpMetrics,
        |n| {
            let mut r = Rng::new(SEED);
            let res = sdk_resource(&mut r);
            metrics_request(res, (0..n).map(|i| metric(&mut r, i)).collect())
        },
        metrics_json,
        |r| {
            use metric::Data;
            r.resource_metrics
                .iter()
                .flat_map(|x| &x.scope_metrics)
                .flat_map(|x| &x.metrics)
                .map(|m| match &m.data {
                    Some(Data::Gauge(g)) => g.data_points.len(),
                    Some(Data::Sum(s)) => s.data_points.len(),
                    Some(Data::Histogram(h)) => h.data_points.len(),
                    _ => 0,
                })
                .sum()
        },
    ));
    v
}
