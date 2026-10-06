//! End-to-end planner and aggregation tests: parse a query string, plan it against a real
//! Tantivy index, and check exactly which fixture events come back.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use event::{Event, EventType, LogLevel};
use query::{execute_aggregation, parse_query, plan_filter, AggregationResult};
use serde_json::{json, Value};
use storage::{EventSearchParams, EventStore};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 2, 1, 10, 0, 0).unwrap()
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<EventStore>,
}

fn log(name: &str, mins: i64, level: LogLevel, service: &str, message: &str, attrs: Value) -> Event {
    let mut e = Event::new_log();
    e.timestamp = t0() + Duration::minutes(mins);
    e.level = Some(level);
    e.service = Some(service.into());
    e.environment = Some("production".into());
    e.message = Some(message.into());
    e.message_template = Some(name.into());
    e.attributes = attrs.as_object().cloned().unwrap_or_default();
    e
}

fn span(name: &str, mins: i64, service: &str, duration_ns: u64) -> Event {
    let mut e = Event::new_span();
    e.timestamp = t0() + Duration::minutes(mins);
    e.service = Some(service.into());
    e.message = Some(format!("span {name}"));
    e.message_template = Some(name.into());
    e.trace_id = Some(format!("trace-{name}"));
    e.duration_ns = Some(duration_ns);
    e
}

/// Each event's `message_template` doubles as a short name so tests can assert on sets of names.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path()).unwrap();
    let mut stack = log("crash", 4, LogLevel::Fatal, "billing", "Process crashed", json!({}));
    stack.stacktrace = Some("NullPointerException at Billing.charge".into());
    stack.environment = Some("staging".into());
    let events = vec![
        log("trace1", 0, LogLevel::Trace, "api", "entering handler", json!({"route": "/orders"})),
        log("debug1", 1, LogLevel::Debug, "api", "cache miss for key", json!({"cacheHit": false})),
        log(
            "info1",
            2,
            LogLevel::Information,
            "api",
            "POST /orders completed",
            json!({"http": {"status_code": 200}, "cacheHit": true, "customerId": 7}),
        ),
        log(
            "warn1",
            3,
            LogLevel::Warning,
            "billing",
            "Database timeout while charging",
            json!({"customerId": 42, "provider": "Stripe", "amount": 19.99}),
        ),
        log(
            "error1",
            3,
            LogLevel::Error,
            "billing",
            "Payment failed",
            json!({"customerId": 42, "provider": "adyen", "retries": 3}),
        ),
        stack,
        span("fast", 5, "api", 1_000_000),
        span("slow", 6, "api", 250_000_000),
        span("medium", 7, "billing", 50_000_000),
    ];
    store.write_batch(&events).unwrap();
    Fixture { _dir: dir, store }
}

impl Fixture {
    fn names(&self, q: &str) -> BTreeSet<String> {
        let parsed = parse_query(q).unwrap_or_else(|e| panic!("parse {q:?}: {e}"));
        let query = plan_filter(self.store.index(), parsed.filter.as_ref())
            .unwrap_or_else(|e| panic!("plan {q:?}: {e}"));
        self.store
            .search(EventSearchParams { query, from: None, to: None, limit: 100, cursor: None })
            .unwrap()
            .events
            .into_iter()
            .map(|e| e.message_template.unwrap())
            .collect()
    }

    fn plan_err(&self, q: &str) -> String {
        let parsed = parse_query(q).unwrap();
        match plan_filter(self.store.index(), parsed.filter.as_ref()) {
            Ok(_) => panic!("expected planning error for {q:?}"),
            Err(e) => e.to_string(),
        }
    }

    fn agg(&self, q: &str) -> AggregationResult {
        self.agg_range(q, None, None)
    }

    fn agg_range(&self, q: &str, from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> AggregationResult {
        let parsed = parse_query(q).unwrap();
        let query = plan_filter(self.store.index(), parsed.filter.as_ref()).unwrap();
        execute_aggregation(&self.store, query, from, to, parsed.aggregation.as_ref().unwrap()).unwrap()
    }
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

const ALL_LOGS: &[&str] = &["trace1", "debug1", "info1", "warn1", "error1", "crash"];

// --- filters ----------------------------------------------------------------

#[test]
fn empty_query_matches_everything() {
    let f = fixture();
    assert_eq!(f.names("").len(), 9);
    assert_eq!(f.names("   ").len(), 9);
}

#[test]
fn equality_on_well_known_fields() {
    let f = fixture();
    assert_eq!(f.names(r#"service = "billing""#), set(&["warn1", "error1", "crash", "medium"]));
    assert_eq!(f.names("environment = staging"), set(&["crash"]));
    assert_eq!(f.names("event_type = span"), set(&["fast", "slow", "medium"]));
    assert_eq!(f.names(r#"trace_id = "trace-slow""#), set(&["slow"]));
}

#[test]
fn not_equal_excludes_matches() {
    let f = fixture();
    assert_eq!(f.names(r#"service != "api""#), set(&["warn1", "error1", "crash", "medium"]));
}

#[test]
fn level_equality_accepts_aliases() {
    let f = fixture();
    assert_eq!(f.names("level = error"), set(&["error1"]));
    assert_eq!(f.names("level = ERR"), set(&["error1"]));
    assert_eq!(f.names("level = warn"), set(&["warn1"]));
    assert_eq!(f.names("level = info"), set(&["info1"]));
    assert_eq!(f.names("level = critical"), set(&["crash"]));
}

#[test]
fn level_ordering_comparisons() {
    let f = fixture();
    assert_eq!(f.names("level >= warning"), set(&["warn1", "error1", "crash"]));
    assert_eq!(f.names("level > warning"), set(&["error1", "crash"]));
    assert_eq!(f.names("level <= debug"), set(&["trace1", "debug1"]));
    assert_eq!(f.names("level < debug"), set(&["trace1"]));
    assert!(f.names("level < trace").is_empty());
    assert!(f.names("level > fatal").is_empty());
}

#[test]
fn level_not_equal_includes_events_without_level() {
    let f = fixture();
    let names = f.names("level != information");
    assert!(!names.contains("info1"));
    assert!(names.contains("slow"), "spans have no level and should match !=");
    assert_eq!(names.len(), 8);
}

#[test]
fn invalid_level_is_a_planning_error() {
    let f = fixture();
    assert!(f.plan_err("level >= loud").contains("Invalid log level"));
}

#[test]
fn duration_comparisons() {
    let f = fixture();
    assert_eq!(f.names("duration_ns > 50000000"), set(&["slow"]));
    assert_eq!(f.names("duration_ns >= 50000000"), set(&["slow", "medium"]));
    assert_eq!(f.names("duration_ns < 50000000"), set(&["fast"]));
    assert_eq!(f.names("duration_ns <= 50000000"), set(&["fast", "medium"]));
    assert_eq!(f.names("duration_ns = 1000000"), set(&["fast"]));
    assert_eq!(f.names(r#"duration_ns = "1000000""#), set(&["fast"]));
}

#[test]
fn duration_requires_numeric_value() {
    let f = fixture();
    assert!(f.plan_err(r#"duration_ns > "slow""#).contains("Expected number"));
    assert!(f.plan_err("duration_ns > true").contains("Expected number"));
}

#[test]
fn attribute_integer_equality() {
    let f = fixture();
    assert_eq!(f.names("customerId = 42"), set(&["warn1", "error1"]));
    assert_eq!(f.names("retries = 3"), set(&["error1"]));
    assert!(f.names("customerId = 999").is_empty());
}

#[test]
fn explicit_attributes_prefix_matches() {
    let f = fixture();
    assert_eq!(f.names("attributes.customerId = 7"), set(&["info1"]));
}

#[test]
fn attribute_not_equal() {
    let f = fixture();
    let names = f.names("customerId != 42");
    assert!(!names.contains("warn1") && !names.contains("error1"));
    assert_eq!(names.len(), 7);
}

#[test]
fn attribute_string_equality_is_case_insensitive() {
    let f = fixture();
    assert_eq!(f.names(r#"provider = "stripe""#), set(&["warn1"]));
    assert_eq!(f.names(r#"provider = "STRIPE""#), set(&["warn1"]));
    assert_eq!(f.names(r#"provider = "Adyen""#), set(&["error1"]));
}

#[test]
fn attribute_bool_equality() {
    let f = fixture();
    assert_eq!(f.names("cacheHit = true"), set(&["info1"]));
    assert_eq!(f.names("cacheHit = false"), set(&["debug1"]));
}

#[test]
fn nested_attribute_path() {
    let f = fixture();
    assert_eq!(f.names("http.status_code = 200"), set(&["info1"]));
}

#[test]
fn attribute_range_is_rejected_with_clear_error() {
    let f = fixture();
    assert!(f.plan_err("customerId > 10").contains("customerId"));
    assert!(f.plan_err(r#"provider > "a""#).contains("integer"));
}

#[test]
fn contains_on_message() {
    let f = fixture();
    assert_eq!(f.names(r#"message contains "timeout""#), set(&["warn1"]));
    assert_eq!(f.names(r#"message contains "TIMEOUT""#), set(&["warn1"]));
    assert_eq!(f.names(r#"message contains "payment failed""#), set(&["error1"]));
    // Phrase must be adjacent and in order.
    assert!(f.names(r#"message contains "failed payment""#).is_empty());
}

#[test]
fn contains_on_attribute() {
    let f = fixture();
    assert_eq!(f.names(r#"provider contains "stripe""#), set(&["warn1"]));
}

#[test]
fn free_text_searches_message_template_and_stacktrace() {
    let f = fixture();
    assert_eq!(f.names("timeout"), set(&["warn1"]));
    assert_eq!(f.names("NullPointerException"), set(&["crash"]));
    // Multiple words are OR'd together.
    assert_eq!(f.names("timeout crashed"), set(&["warn1", "crash"]));
    assert!(f.names("nonexistentword").is_empty());
}

#[test]
fn boolean_combinations() {
    let f = fixture();
    assert_eq!(f.names(r#"service = "billing" and level = error"#), set(&["error1"]));
    assert_eq!(f.names("level = error or level = trace"), set(&["error1", "trace1"]));
    assert_eq!(
        f.names(r#"(service = "api" or service = "billing") and level >= warning"#),
        set(&["warn1", "error1", "crash"])
    );
    assert_eq!(f.names(r#"not service = "api""#), set(&["warn1", "error1", "crash", "medium"]));
    assert_eq!(
        f.names(r#"event_type = log and not level >= warning"#),
        set(&["trace1", "debug1", "info1"])
    );
}

#[test]
fn and_binds_tighter_than_or() {
    let f = fixture();
    // trace1 OR (billing AND error) — not (trace1 OR billing) AND error.
    assert_eq!(
        f.names(r#"level = trace or service = "billing" and level = error"#),
        set(&["trace1", "error1"])
    );
}

#[test]
fn time_filters_apply_with_query() {
    let f = fixture();
    let parsed = parse_query("event_type = log").unwrap();
    let query = plan_filter(f.store.index(), parsed.filter.as_ref()).unwrap();
    let names: BTreeSet<_> = f
        .store
        .search(EventSearchParams {
            query,
            from: Some(t0() + Duration::minutes(2)),
            to: Some(t0() + Duration::minutes(3)),
            limit: 100,
            cursor: None,
        })
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.message_template.unwrap())
        .collect();
    assert_eq!(names, set(&["info1", "warn1", "error1"]));
    assert!(ALL_LOGS.iter().all(|n| f.names("event_type = log").contains(*n)));
}

// --- aggregations -----------------------------------------------------------

fn number(r: AggregationResult) -> f64 {
    match r {
        AggregationResult::Number { value } => value,
        other => panic!("expected number, got {other:?}"),
    }
}

fn groups(r: AggregationResult) -> Vec<(String, f64)> {
    match r {
        AggregationResult::Groups { points } => points.into_iter().map(|p| (p.key, p.value)).collect(),
        other => panic!("expected groups, got {other:?}"),
    }
}

#[test]
fn count() {
    let f = fixture();
    assert_eq!(number(f.agg("| count")), 9.0);
    assert_eq!(number(f.agg("level >= warning | count")), 3.0);
    assert_eq!(number(f.agg(r#"service = "nope" | count"#)), 0.0);
}

#[test]
fn count_with_time_range() {
    let f = fixture();
    let v = number(f.agg_range(
        "| count",
        Some(t0() + Duration::minutes(5)),
        Some(t0() + Duration::minutes(6)),
    ));
    assert_eq!(v, 2.0);
}

#[test]
fn count_by_field_is_sorted_and_includes_null_bucket() {
    let f = fixture();
    assert_eq!(
        groups(f.agg("| count by service")),
        vec![("api".into(), 5.0), ("billing".into(), 4.0)]
    );
    assert_eq!(
        groups(f.agg("| count by level")),
        vec![
            ("(null)".into(), 3.0),
            ("debug".into(), 1.0),
            ("error".into(), 1.0),
            ("fatal".into(), 1.0),
            ("information".into(), 1.0),
            ("trace".into(), 1.0),
            ("warning".into(), 1.0),
        ]
    );
}

#[test]
fn count_by_attribute_and_nested_attribute() {
    let f = fixture();
    let g = groups(f.agg("event_type = log | count by customerId"));
    assert_eq!(g, vec![("(null)".into(), 3.0), ("42".into(), 2.0), ("7".into(), 1.0)]);
    let g = groups(f.agg("customerId = 7 | count by http.status_code"));
    assert_eq!(g, vec![("200".into(), 1.0)]);
}

#[test]
fn count_by_time_buckets() {
    let f = fixture();
    let r = f.agg("event_type = log | count by time(5m)");
    let AggregationResult::TimeSeries { points, interval } = r else {
        panic!("expected time series");
    };
    assert_eq!(interval, "5m");
    let keys: Vec<_> = points.iter().map(|p| (p.key.clone(), p.value)).collect();
    // All six logs fall in 10:00..10:05.
    assert_eq!(keys, vec![(t0().to_rfc3339(), 6.0)]);

    let r = f.agg("| count by time(1m)");
    let AggregationResult::TimeSeries { points, .. } = r else { panic!() };
    assert_eq!(points.len(), 8, "one bucket per distinct minute 0..=7");
    assert_eq!(points.iter().map(|p| p.value).sum::<f64>(), 9.0);
    let at3 = points.iter().find(|p| p.key == (t0() + Duration::minutes(3)).to_rfc3339()).unwrap();
    assert_eq!(at3.value, 2.0);
    assert!(points.windows(2).all(|w| w[0].key < w[1].key), "buckets are chronological");
}

#[test]
fn numeric_aggregations_over_duration() {
    let f = fixture();
    let mean = (1_000_000.0 + 250_000_000.0 + 50_000_000.0) / 3.0;
    assert!((number(f.agg("event_type = span | avg(duration_ns)")) - mean).abs() < 1e-6);
    assert_eq!(number(f.agg("| sum(duration_ns)")), 301_000_000.0);
    assert_eq!(number(f.agg("| min(duration_ns)")), 1_000_000.0);
    assert_eq!(number(f.agg("| max duration_ns")), 250_000_000.0);
}

#[test]
fn numeric_aggregations_over_attributes() {
    let f = fixture();
    assert_eq!(number(f.agg("| sum(customerId)")), 91.0);
    assert_eq!(number(f.agg("| max(amount)")), 19.99);
    assert_eq!(number(f.agg("| avg(retries)")), 3.0);
}

#[test]
fn numeric_aggregations_with_no_values_return_zero() {
    let f = fixture();
    assert_eq!(number(f.agg("| avg(nope)")), 0.0);
    assert_eq!(number(f.agg("| sum(nope)")), 0.0);
    assert_eq!(number(f.agg("| min(nope)")), 0.0);
    assert_eq!(number(f.agg("| max(nope)")), 0.0);
}

#[test]
fn aggregation_result_serialises_with_type_tag() {
    let f = fixture();
    let v = serde_json::to_value(f.agg("| count")).unwrap();
    assert_eq!(v, json!({"type": "number", "value": 9.0}));
    let v = serde_json::to_value(f.agg("| count by time(1h)")).unwrap();
    assert_eq!(v["type"], "timeSeries");
    assert_eq!(v["interval"], "1h");
    let v = serde_json::to_value(f.agg("| count by service")).unwrap();
    assert_eq!(v["type"], "groups");
    assert_eq!(v["points"][0], json!({"key": "api", "value": 5.0}));
}

#[test]
fn spans_have_event_type_span() {
    let f = fixture();
    let parsed = parse_query("duration_ns > 0").unwrap();
    let query = plan_filter(f.store.index(), parsed.filter.as_ref()).unwrap();
    let events = f
        .store
        .search(EventSearchParams { query, from: None, to: None, limit: 10, cursor: None })
        .unwrap()
        .events;
    assert!(events.iter().all(|e| e.event_type == EventType::Span));
}
