use std::collections::HashSet;

use chrono::{DateTime, Duration, TimeZone, Utc};
use event::{Event, EventType, LogLevel};
use serde_json::{json, Map, Value};
use storage::{EventSearchParams, EventStore, SearchCursor};
use tantivy::query::AllQuery;
use uuid::Uuid;

fn base_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap()
}

fn log_at(ts: DateTime<Utc>, message: &str) -> Event {
    let mut e = Event::new_log();
    e.timestamp = ts;
    e.message = Some(message.into());
    e
}

fn all(limit: usize) -> EventSearchParams {
    EventSearchParams {
        query: Box::new(AllQuery),
        from: None,
        to: None,
        limit,
        cursor: None,
    }
}

fn open() -> (tempfile::TempDir, std::sync::Arc<EventStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store = EventStore::open(dir.path()).unwrap();
    (dir, store)
}

#[test]
fn empty_store_has_no_events() {
    let (_dir, store) = open();
    assert_eq!(store.event_count().unwrap(), 0);
    let result = store.search(all(10)).unwrap();
    assert!(result.events.is_empty());
    assert!(result.next_cursor.is_none());
    assert_eq!(result.total_estimate, 0);
}

#[test]
fn write_empty_batch_is_noop() {
    let (_dir, store) = open();
    store.write_batch(&[]).unwrap();
    assert_eq!(store.event_count().unwrap(), 0);
}

#[test]
fn round_trips_every_field() {
    let (_dir, store) = open();
    // Stored timestamps keep microsecond precision (sub-microsecond digits are dropped).
    let ts = Utc.with_ymd_and_hms(2026, 3, 1, 8, 30, 15).unwrap() + Duration::microseconds(123_456);
    let attrs = json!({
        "customerId": 42,
        "negative": -7,
        "ratio": 0.25,
        "ok": true,
        "provider": "stripe",
        "tags": ["a", "b"],
        "nested": {"region": "eu", "zone": 3}
    });
    let event = Event {
        id: Uuid::new_v4(),
        timestamp: ts,
        event_type: EventType::Span,
        level: Some(LogLevel::Warning),
        message: Some("GET /orders".into()),
        message_template: Some("GET {route}".into()),
        service: Some("api".into()),
        environment: Some("staging".into()),
        trace_id: Some("trace-abc".into()),
        span_id: Some("span-1".into()),
        parent_span_id: Some("span-0".into()),
        duration_ns: Some(1_500_000),
        stacktrace: Some("at a()\nat b()".into()),
        attributes: attrs.as_object().unwrap().clone(),
    };
    store.write_batch(std::slice::from_ref(&event)).unwrap();

    let result = store.search(all(10)).unwrap();
    assert_eq!(result.events.len(), 1);
    let got = &result.events[0];
    assert_eq!(got.id, event.id);
    assert_eq!(got.timestamp, ts, "microsecond timestamp precision must survive");
    assert_eq!(got.event_type, EventType::Span);
    assert_eq!(got.level, Some(LogLevel::Warning));
    assert_eq!(got.message, event.message);
    assert_eq!(got.message_template, event.message_template);
    assert_eq!(got.service, event.service);
    assert_eq!(got.environment, event.environment);
    assert_eq!(got.trace_id, event.trace_id);
    assert_eq!(got.span_id, event.span_id);
    assert_eq!(got.parent_span_id, event.parent_span_id);
    assert_eq!(got.duration_ns, Some(1_500_000));
    assert_eq!(got.stacktrace, event.stacktrace);
    assert_eq!(Value::Object(got.attributes.clone()), attrs);
}

#[test]
fn optional_fields_stay_none() {
    let (_dir, store) = open();
    let mut e = Event::new_span();
    e.timestamp = base_time();
    store.write_batch(&[e]).unwrap();

    let got = &store.search(all(1)).unwrap().events[0];
    assert_eq!(got.level, None);
    assert_eq!(got.message, None);
    assert_eq!(got.service, None);
    assert_eq!(got.duration_ns, None);
    assert_eq!(got.attributes, Map::new());
}

#[test]
fn results_are_newest_first() {
    let (_dir, store) = open();
    let t = base_time();
    store
        .write_batch(&[
            log_at(t + Duration::seconds(1), "second"),
            log_at(t + Duration::seconds(3), "fourth"),
            log_at(t, "first"),
            log_at(t + Duration::seconds(2), "third"),
        ])
        .unwrap();

    let messages: Vec<_> = store
        .search(all(10))
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.message.unwrap())
        .collect();
    assert_eq!(messages, ["fourth", "third", "second", "first"]);
}

#[test]
fn time_range_bounds_are_inclusive() {
    let (_dir, store) = open();
    let t = base_time();
    store
        .write_batch(&(0..5).map(|i| log_at(t + Duration::minutes(i), &format!("m{i}"))).collect::<Vec<_>>())
        .unwrap();

    let result = store
        .search(EventSearchParams {
            from: Some(t + Duration::minutes(1)),
            to: Some(t + Duration::minutes(3)),
            ..all(10)
        })
        .unwrap();
    let messages: Vec<_> = result.events.iter().map(|e| e.message.clone().unwrap()).collect();
    assert_eq!(messages, ["m3", "m2", "m1"]);
    assert_eq!(result.total_estimate, 3);

    let only_from = store
        .search(EventSearchParams { from: Some(t + Duration::minutes(4)), ..all(10) })
        .unwrap();
    assert_eq!(only_from.events.len(), 1);

    let only_to = store
        .search(EventSearchParams { to: Some(t), ..all(10) })
        .unwrap();
    assert_eq!(only_to.events.len(), 1);
}

#[test]
fn limit_truncates_and_reports_total() {
    let (_dir, store) = open();
    let t = base_time();
    store
        .write_batch(&(0..10).map(|i| log_at(t + Duration::seconds(i), "x")).collect::<Vec<_>>())
        .unwrap();
    let result = store.search(all(3)).unwrap();
    assert_eq!(result.events.len(), 3);
    assert_eq!(result.total_estimate, 10);
    assert!(result.next_cursor.is_some());
}

#[test]
fn no_cursor_when_results_fit_exactly() {
    let (_dir, store) = open();
    let t = base_time();
    store
        .write_batch(&(0..3).map(|i| log_at(t + Duration::seconds(i), "x")).collect::<Vec<_>>())
        .unwrap();
    let result = store.search(all(3)).unwrap();
    assert_eq!(result.events.len(), 3);
    assert!(result.next_cursor.is_none());
}

fn paginate(store: &EventStore, page_size: usize) -> Vec<Event> {
    let mut seen = Vec::new();
    let mut cursor = None;
    for _ in 0..1000 {
        let result = store
            .search(EventSearchParams { cursor, ..all(page_size) })
            .unwrap();
        assert!(result.events.len() <= page_size);
        seen.extend(result.events);
        match result.next_cursor {
            Some(c) => cursor = Some(SearchCursor::decode(&c).unwrap()),
            None => return seen,
        }
    }
    panic!("pagination did not terminate");
}

#[test]
fn cursor_pagination_visits_every_event_once() {
    let (_dir, store) = open();
    let t = base_time();
    let events: Vec<_> = (0..23).map(|i| log_at(t + Duration::seconds(i), &format!("e{i}"))).collect();
    store.write_batch(&events).unwrap();

    let seen = paginate(&store, 5);
    let ids: HashSet<_> = seen.iter().map(|e| e.id).collect();
    assert_eq!(seen.len(), 23, "no duplicates");
    assert_eq!(ids.len(), 23, "every event visited");
    assert!(seen.windows(2).all(|w| w[0].timestamp >= w[1].timestamp));
}

#[test]
fn cursor_pagination_handles_identical_timestamps() {
    // Bursts of events commonly share a timestamp (bulk ingest, batch log flushes).
    let (_dir, store) = open();
    let t = base_time();
    let events: Vec<_> = (0..12).map(|i| log_at(t, &format!("same{i}"))).collect();
    store.write_batch(&events).unwrap();

    let seen = paginate(&store, 5);
    let ids: HashSet<_> = seen.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 12, "every event visited exactly once; got {} rows", seen.len());
    assert_eq!(seen.len(), 12);
}

#[test]
fn cursor_encode_decode_round_trip() {
    let cursor = SearchCursor {
        timestamp_nanos: 1_700_000_000_123_456_789,
        id: Uuid::new_v4().to_string(),
    };
    let encoded = cursor.encode();
    assert!(encoded.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    let decoded = SearchCursor::decode(&encoded).unwrap();
    assert_eq!(decoded.timestamp_nanos, cursor.timestamp_nanos);
    assert_eq!(decoded.id, cursor.id);
}

#[test]
fn cursor_decode_rejects_garbage() {
    assert!(SearchCursor::decode("not base64!!").is_err());
    assert!(SearchCursor::decode("aGVsbG8").is_err()); // valid base64, not JSON
    assert!(SearchCursor::decode("").is_err());
}

#[test]
fn delete_before_removes_only_older_events() {
    let (_dir, store) = open();
    let t = base_time();
    store
        .write_batch(&[
            log_at(t - Duration::days(2), "old"),
            log_at(t - Duration::seconds(1), "just-before"),
            log_at(t, "at-cutoff"),
            log_at(t + Duration::days(1), "new"),
        ])
        .unwrap();

    store.delete_before(t).unwrap();

    let mut remaining: Vec<_> = store
        .search(all(10))
        .unwrap()
        .events
        .into_iter()
        .map(|e| e.message.unwrap())
        .collect();
    remaining.sort();
    assert_eq!(remaining, ["at-cutoff", "new"]);
    assert_eq!(store.event_count().unwrap(), 2);
}

#[test]
fn data_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let id;
    {
        let store = EventStore::open(dir.path()).unwrap();
        let e = log_at(base_time(), "persisted");
        id = e.id;
        store.write_batch(&[e]).unwrap();
    }
    let store = EventStore::open(dir.path()).unwrap();
    let events = store.search(all(10)).unwrap().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, id);
}

#[test]
fn open_creates_missing_directories() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("a").join("b").join("index");
    let store = EventStore::open(&nested).unwrap();
    store.write_batch(&[log_at(base_time(), "x")]).unwrap();
    assert!(nested.join("meta.json").exists());
}


#[test]
fn second_page_continues_after_first() {
    // Narrower regression guard that passes today: page 2 starts right after page 1.
    let (_dir, store) = open();
    let t = base_time();
    let events: Vec<_> = (0..10).map(|i| log_at(t + Duration::seconds(i), &format!("e{i}"))).collect();
    store.write_batch(&events).unwrap();

    let page1 = store.search(all(4)).unwrap();
    let cursor = SearchCursor::decode(page1.next_cursor.as_deref().unwrap()).unwrap();
    let page2 = store.search(EventSearchParams { cursor: Some(cursor), ..all(4) }).unwrap();
    let names: Vec<_> = page1.events.iter().chain(&page2.events).map(|e| e.message.clone().unwrap()).collect();
    assert_eq!(names, ["e9", "e8", "e7", "e6", "e5", "e4", "e3", "e2"]);
}

#[test]
fn sub_second_events_are_ordered() {
    let (_dir, store) = open();
    let t = base_time();
    let events: Vec<_> = (0..10).map(|i| log_at(t + Duration::milliseconds(i * 100), &format!("ms{i}"))).collect();
    store.write_batch(&events).unwrap();
    let names: Vec<_> = store.search(all(10)).unwrap().events.into_iter().map(|e| e.message.unwrap()).collect();
    let expected: Vec<_> = (0..10).rev().map(|i| format!("ms{i}")).collect();
    assert_eq!(names, expected);
}

#[test]
fn sub_second_time_range_is_respected() {
    let (_dir, store) = open();
    let t = base_time();
    let events: Vec<_> = (0..10).map(|i| log_at(t + Duration::milliseconds(i * 100), &format!("ms{i}"))).collect();
    store.write_batch(&events).unwrap();
    let result = store
        .search(EventSearchParams { from: Some(t + Duration::milliseconds(450)), ..all(10) })
        .unwrap();
    assert_eq!(result.events.len(), 5);
}

#[test]
fn incompatible_existing_index_is_replaced() {
    // An index written with an older schema (e.g. second-precision timestamps) is wiped and
    // recreated rather than preventing startup.
    let dir = tempfile::tempdir().unwrap();
    {
        let mut builder = tantivy::schema::Schema::builder();
        let f = builder.add_text_field("legacy", tantivy::schema::STRING | tantivy::schema::STORED);
        let index = tantivy::Index::create_in_dir(dir.path(), builder.build()).unwrap();
        let mut writer: tantivy::IndexWriter = index.writer(15_000_000).unwrap();
        writer.add_document(tantivy::doc!(f => "old")).unwrap();
        writer.commit().unwrap();
    }
    let store = EventStore::open(dir.path()).unwrap();
    assert_eq!(store.event_count().unwrap(), 0);
    store.write_batch(&[log_at(base_time(), "fresh")]).unwrap();
    assert_eq!(store.search(all(10)).unwrap().events[0].message.as_deref(), Some("fresh"));
}

#[test]
fn pagination_with_mixed_ties_and_filters() {
    let (_dir, store) = open();
    let t = base_time();
    // Groups of 1..=4 events per timestamp, plus a time filter that cuts one group off.
    let mut events = Vec::new();
    for second in 0..8 {
        for n in 0..(second % 4 + 1) {
            events.push(log_at(t + Duration::seconds(second), &format!("s{second}n{n}")));
        }
    }
    store.write_batch(&events).unwrap();

    for page_size in [1, 2, 3, 5, 7, 100] {
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let result = store
                .search(EventSearchParams { cursor, from: Some(t + Duration::seconds(1)), ..all(page_size) })
                .unwrap();
            seen.extend(result.events);
            match result.next_cursor {
                Some(c) => cursor = Some(SearchCursor::decode(&c).unwrap()),
                None => break,
            }
        }
        let expected = events.iter().filter(|e| e.timestamp >= t + Duration::seconds(1)).count();
        let ids: HashSet<_> = seen.iter().map(|e| e.id).collect();
        assert_eq!(seen.len(), expected, "page size {page_size}");
        assert_eq!(ids.len(), expected, "page size {page_size}");
        assert!(seen.windows(2).all(|w| (w[0].timestamp, w[0].id.to_string()) > (w[1].timestamp, w[1].id.to_string())));
    }
}

#[test]
fn sub_second_pagination_and_retention() {
    let (_dir, store) = open();
    let t = base_time();
    // Several events per millisecond-ish slot, all within one second.
    let events: Vec<_> = (0..30)
        .map(|i| log_at(t + Duration::microseconds((i / 3) * 1500), &format!("u{i}")))
        .collect();
    store.write_batch(&events).unwrap();

    let seen = paginate(&store, 4);
    let ids: HashSet<_> = seen.iter().map(|e| e.id).collect();
    assert_eq!((seen.len(), ids.len()), (30, 30));
    assert!(seen.windows(2).all(|w| w[0].timestamp >= w[1].timestamp));

    let to = store
        .search(EventSearchParams { to: Some(t + Duration::microseconds(1500)), ..all(100) })
        .unwrap();
    assert_eq!(to.events.len(), 6);

    store.delete_before(t + Duration::microseconds(3000)).unwrap();
    assert_eq!(store.event_count().unwrap(), 24);
}
