use chrono::{Duration, Utc};
use event::{Event, EventType, LogLevel};
use query::{parse_query, plan_filter};
use serde_json::json;
use storage::{EventSearchParams, EventStore};
use uuid::Uuid;

#[test]
fn ingest_query_filter_and_attributes() {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path()).unwrap();

    let now = Utc::now();
    let events = vec![
        make_log(
            now - Duration::seconds(10),
            LogLevel::Error,
            "billing",
            "Payment failed",
            json!({"customerId": 42, "provider": "stripe"}),
        ),
        make_log(
            now - Duration::seconds(5),
            LogLevel::Information,
            "api",
            "POST /orders completed",
            json!({"http.status_code": 200}),
        ),
        make_span(now - Duration::seconds(8), "api", "GET /orders", "trace-1", 50_000_000),
    ];
    store.write_batch(&events).unwrap();

    let parsed = parse_query(r#"service = "billing""#).unwrap();
    let q = plan_filter(store.index(), parsed.filter.as_ref()).unwrap();
    let result = store
        .search(EventSearchParams {
            query: q,
            from: Some(now - Duration::hours(1)),
            to: Some(now + Duration::minutes(1)),
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0].message.as_deref(), Some("Payment failed"));

    let parsed = parse_query("customerId = 42").unwrap();
    let q = plan_filter(store.index(), parsed.filter.as_ref()).unwrap();
    let result = store
        .search(EventSearchParams {
            query: q,
            from: None,
            to: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(result.events.len(), 1);
    assert_eq!(
        result.events[0].attributes.get("provider").and_then(|v| v.as_str()),
        Some("stripe")
    );

    let parsed = parse_query("level >= warning").unwrap();
    let q = plan_filter(store.index(), parsed.filter.as_ref()).unwrap();
    let result = store
        .search(EventSearchParams {
            query: q,
            from: None,
            to: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert!(result.events.iter().any(|e| e.level == Some(LogLevel::Error)));

    let parsed = parse_query(r#"trace_id = "trace-1""#).unwrap();
    let q = plan_filter(store.index(), parsed.filter.as_ref()).unwrap();
    let result = store
        .search(EventSearchParams {
            query: q,
            from: None,
            to: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(result.events.len(), 1);
    assert_eq!(result.events[0].event_type, EventType::Span);
}

fn make_log(
    ts: chrono::DateTime<Utc>,
    level: LogLevel,
    service: &str,
    message: &str,
    attrs: serde_json::Value,
) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: ts,
        event_type: EventType::Log,
        level: Some(level),
        message: Some(message.into()),
        message_template: None,
        service: Some(service.into()),
        environment: Some("production".into()),
        trace_id: None,
        span_id: None,
        parent_span_id: None,
        duration_ns: None,
        attributes: attrs.as_object().cloned().unwrap_or_default(),
        stacktrace: None,
    }
}

fn make_span(
    ts: chrono::DateTime<Utc>,
    service: &str,
    name: &str,
    trace_id: &str,
    duration_ns: u64,
) -> Event {
    Event {
        id: Uuid::new_v4(),
        timestamp: ts,
        event_type: EventType::Span,
        level: None,
        message: Some(name.into()),
        message_template: None,
        service: Some(service.into()),
        environment: Some("production".into()),
        trace_id: Some(trace_id.into()),
        span_id: Some("span-1".into()),
        parent_span_id: None,
        duration_ns: Some(duration_ns),
        attributes: Default::default(),
        stacktrace: None,
    }
}
