//! Proptest strategies for arbitrary telemetry. Enabled by the `testing`
//! feature and used by storage/query property tests.

use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;

use crate::*;

fn small_string() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z]{0,8}",
        "[ -~]{0,24}",
        // A little unicode to catch byte/char confusion.
        "[a-zé漢🙂]{0,6}",
    ]
}

fn key() -> impl Strategy<Value = String> {
    prop_oneof!["[a-z]{1,6}", "[a-z]{1,4}\\.[a-z]{1,4}"]
}

pub fn value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        // Finite floats only: NaN != NaN would make equality assertions useless.
        (-1e12f64..1e12).prop_map(Value::Float),
        small_string().prop_map(Value::String),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..4).prop_map(Value::Array),
            vec((key(), inner), 0..4).prop_map(|kv| Value::Object(kv.into_iter().collect())),
        ]
    })
}

pub fn fields() -> impl Strategy<Value = Fields> {
    vec((key(), value()), 0..6).prop_map(|kv| kv.into_iter().collect())
}

fn level() -> impl Strategy<Value = Level> {
    proptest::sample::select(Level::ALL.to_vec())
}

fn exception() -> impl Strategy<Value = Exception> {
    (option::of(small_string()), option::of(small_string()), option::of(small_string()))
        .prop_map(|(kind, message, stack_trace)| Exception { kind, message, stack_trace })
}

pub fn log_payload() -> impl Strategy<Value = TelemetryPayload> {
    (level(), small_string(), option::of(small_string()), option::of(exception())).prop_map(
        |(level, message, message_template, exception)| {
            TelemetryPayload::Log(LogEvent { level, message, message_template, exception })
        },
    )
}

fn hex_id(len: usize) -> impl Strategy<Value = String> {
    proptest::string::string_regex(&format!("[0-9a-f]{{{len}}}")).unwrap()
}

pub fn span_payload() -> impl Strategy<Value = TelemetryPayload> {
    (
        option::of(hex_id(16)),
        small_string(),
        0u8..6,
        any::<u64>(),
        0u8..3,
        option::of(small_string()),
        vec((any::<i64>(), small_string(), fields()), 0..3),
        vec((hex_id(32), hex_id(16), fields()), 0..2),
    )
        .prop_map(|(parent, name, kind, duration_nanos, code, msg, events, links)| {
            TelemetryPayload::Span(SpanEvent {
                parent_span_id: parent.and_then(|p| SpanId::new(&p)),
                name,
                kind: SpanKind::from_code(kind),
                duration_nanos,
                status: SpanStatus { code: StatusCode::from_code(code), message: msg },
                events: events
                    .into_iter()
                    .map(|(t, name, attributes)| SpanEventItem { timestamp: Timestamp(t), name, attributes })
                    .collect(),
                links: links
                    .into_iter()
                    .filter_map(|(t, s, attributes)| {
                        Some(SpanLink { trace_id: TraceId::new(&t)?, span_id: SpanId::new(&s)?, attributes })
                    })
                    .collect(),
            })
        })
}

pub fn metric_payload() -> impl Strategy<Value = TelemetryPayload> {
    let number = (-1e9f64..1e9).prop_map(MetricValue::Number);
    let hist = (
        any::<u64>(),
        option::of(-1e9f64..1e9),
        option::of(-1e9f64..1e9),
        option::of(-1e9f64..1e9),
        vec(any::<u64>(), 0..5),
        vec(-1e6f64..1e6, 0..4),
    )
        .prop_map(|(count, sum, min, max, bucket_counts, explicit_bounds)| {
            MetricValue::Histogram(Histogram { count, sum, min, max, bucket_counts, explicit_bounds })
        });
    (
        small_string(),
        option::of(small_string()),
        option::of(small_string()),
        1u8..4,
        0u8..3,
        any::<bool>(),
        prop_oneof![number, hist],
    )
        .prop_map(|(name, description, unit, kind, temporality, monotonic, value)| {
            TelemetryPayload::Metric(MetricEvent {
                name,
                description,
                unit,
                kind: MetricKind::from_code(kind).unwrap(),
                temporality: Temporality::from_code(temporality),
                monotonic,
                value,
            })
        })
}

/// Arbitrary envelope around the given payload strategy.
pub fn event_with(payload: impl Strategy<Value = TelemetryPayload>) -> impl Strategy<Value = TelemetryEvent> {
    (
        any::<u64>(),
        // Keep timestamps in a sane window so deltas stay interesting.
        1_600_000_000_000_000_000i64..1_900_000_000_000_000_000,
        any::<i64>(),
        option::of(proptest::sample::select(vec!["payments", "orders", "auth"])),
        option::of(proptest::sample::select(vec!["production", "staging"])),
        option::of(hex_id(32)),
        option::of(hex_id(16)),
        fields(),
        fields(),
        payload,
    )
        .prop_map(|(id, ts, observed, service, env, trace, span, resource, attributes, payload)| TelemetryEvent {
            id: EventId(id),
            timestamp: Timestamp(ts),
            observed_timestamp: Timestamp(observed),
            service: service.map(String::from),
            environment: env.map(String::from),
            trace_id: trace.and_then(|t| TraceId::new(&t)),
            span_id: span.and_then(|s| SpanId::new(&s)),
            resource,
            attributes,
            payload,
        })
}

pub fn any_event() -> impl Strategy<Value = TelemetryEvent> {
    prop_oneof![event_with(log_payload()), event_with(span_payload()), event_with(metric_payload()),]
}
