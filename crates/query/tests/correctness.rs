//! Storage correctness: indexed query results must equal a brute-force scan.
//!
//! False positives in indexes are fine (they only cost time); a false
//! negative would silently drop data. These tests generate messy datasets,
//! store them across several segments (small blocks so pruning has work to
//! do) plus an unsealed tail, and compare every query against evaluating the
//! same predicate over every event.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use query::ast::{Expr, Literal, Op};
use query::eval::{compile, eval};
use query::{Direction, Engine, QueryLimits, TimeRange};
use storage::segment::SegmentWriteOptions;
use storage::{Storage, StorageConfig};
use telemetry::*;

const BASE: i64 = 1_800_000_000_000_000_000;

fn messy_event(i: usize, rng: &mut impl FnMut(u32) -> u32) -> TelemetryEvent {
    let level = Level::ALL[rng(6) as usize];
    let services = ["payments", "orders", "auth", "123"];
    let mut attrs = Fields::new();
    // Same field, different types across events.
    match rng(5) {
        0 => attrs.insert("customerId", Value::Int(rng(20) as i64)),
        1 => attrs.insert("customerId", Value::from(rng(20).to_string())),
        2 => attrs.insert("customerId", Value::Float(rng(20) as f64)),
        3 => attrs.insert("customerId", Value::from(format!("c{}", rng(20)))),
        _ => {}
    }
    if rng(2) == 0 {
        attrs.insert("durationMs", Value::Int(rng(1000) as i64));
    } else if rng(3) == 0 {
        attrs.insert("durationMs", Value::from(format!("{}", rng(1000))));
    }
    // Nested vs dotted keys for the same logical field.
    if rng(2) == 0 {
        let mut http = Fields::new();
        http.insert("statusCode", Value::Int([200, 404, 500][rng(3) as usize]));
        attrs.insert("http", Value::Object(http));
    } else if rng(2) == 0 {
        attrs.insert("http.statusCode", Value::from(["200", "500"][rng(2) as usize]));
    }
    if rng(3) == 0 {
        attrs.insert(
            "tags",
            Value::Array(vec![Value::from(["red", "green"][rng(2) as usize]), Value::Int(rng(3) as i64)]),
        );
    }
    match rng(4) {
        0 => attrs.insert("flag", Value::Bool(rng(2) == 0)),
        1 => attrs.insert("flag", Value::from(["true", "FALSE", "maybe"][rng(3) as usize])),
        2 => attrs.insert("flag", Value::Null),
        _ => {}
    }
    let mut resource = Fields::new();
    if rng(2) == 0 {
        resource.insert("region", Value::from(["eu", "us"][rng(2) as usize]));
    }
    if rng(4) == 0 {
        // Resource fallback for customerId when attributes lack it.
        resource.insert("customerId", Value::Int(rng(20) as i64));
    }
    TelemetryEvent {
        id: EventId(0),
        // Mostly increasing with jitter so segments overlap in time.
        timestamp: Timestamp(BASE + (i as i64) * 1_000_000_000 + rng(5_000) as i64 * 1_000_000 - 2_000_000_000),
        observed_timestamp: Timestamp(BASE),
        service: (rng(5) != 0).then(|| services[rng(4) as usize].to_string()),
        environment: (rng(2) == 0).then(|| ["production", "staging"][rng(2) as usize].to_string()),
        trace_id: (rng(2) == 0).then(|| TraceId::new(&format!("{:032x}", rng(30) + 1)).unwrap()),
        span_id: None,
        resource,
        attributes: attrs,
        payload: TelemetryPayload::Log(LogEvent {
            level,
            message: format!(
                "{} request {}",
                ["Payment failed", "Order created", "Timeout talking to DB", "user login"][rng(4) as usize],
                rng(100)
            ),
            message_template: Some(["tpl-a", "tpl-b"][rng(2) as usize].to_string()),
            exception: (rng(4) == 0).then(|| Exception {
                kind: Some(["TimeoutException", "IOException"][rng(2) as usize].into()),
                message: None,
                stack_trace: None,
            }),
        }),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    engine: Engine,
    all: Vec<TelemetryEvent>,
}

fn fixture(seed: u64, n: usize) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = StorageConfig::new(dir.path());
    cfg.maintenance_interval = Duration::from_secs(3600);
    cfg.compaction.enabled = false;
    cfg.segment = SegmentWriteOptions {
        block_events: 16,
        // Track few zone fields so "untracked" paths are exercised too.
        max_zone_fields: 3,
        ..Default::default()
    };
    let storage = Arc::new(Storage::open(cfg, None).unwrap());
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut rng = move |m: u32| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) as u32) % m.max(1)
    };
    let events: Vec<TelemetryEvent> = (0..n).map(|i| messy_event(i, &mut rng)).collect();
    // Several segments, then an unsealed tail.
    for (k, chunk) in events.chunks(n / 4 + 1).enumerate() {
        let (tx, rx) = mpsc::sync_channel(1);
        storage.submit(Signal::Logs, chunk.to_vec(), Box::new(move |r| tx.send(r).unwrap())).unwrap();
        rx.recv().unwrap().unwrap();
        if k < 3 {
            storage.rotate_blocking(Signal::Logs).unwrap();
        }
    }
    let snap = storage.snapshot(Signal::Logs);
    let mut all = Vec::new();
    for s in &snap.segments {
        all.extend(s.read_all().unwrap());
    }
    for b in &snap.batches {
        all.extend(b.events.iter().cloned());
    }
    assert_eq!(all.len(), n);
    assert!(snap.segments.len() >= 3);
    Fixture { _dir: dir, engine: Engine::new(storage, QueryLimits::default()), all }
}

fn comparison() -> impl Strategy<Value = Expr> {
    let fields = vec![
        "level",
        "service",
        "environment",
        "message",
        "traceId",
        "customerId",
        "durationMs",
        "http.statusCode",
        "tags",
        "flag",
        "region",
        "resource.customerId",
        "attributes.customerId",
        "missing",
        "messageTemplate",
        "exception.type",
        "timestamp",
    ];
    let lits = prop_oneof![
        proptest::sample::select(vec![
            "Error",
            "Warning",
            "information",
            "payments",
            "orders",
            "123",
            "production",
            "c3",
            "5",
            "500",
            "red",
            "true",
            "false",
            "eu",
            "tpl-a",
            "TimeoutException",
            "timeout",
            "fail",
            "bogus",
            "",
        ])
        .prop_map(|s| Literal::String(s.to_string())),
        (0i64..25).prop_map(Literal::Int),
        proptest::sample::select(vec![200i64, 404, 500, 999]).prop_map(Literal::Int),
        (0.0f64..1000.0).prop_map(Literal::Float),
        any::<bool>().prop_map(Literal::Bool),
        Just(Literal::Null),
        (0i64..200).prop_map(|s| Literal::String(Timestamp(BASE + s * 1_000_000_000).to_rfc3339())),
        Just(Literal::String("0000000000000000000000000000000a".into())),
        Just(Literal::String("0000000000000000000000000000000A".into())),
    ];
    let ops =
        proptest::sample::select(vec![Op::Eq, Op::Eq, Op::Eq, Op::Ne, Op::Gt, Op::Ge, Op::Lt, Op::Le, Op::Contains]);
    (proptest::sample::select(fields), ops, lits)
        .prop_filter("contains null", |(_, o, l)| !(*o == Op::Contains && *l == Literal::Null))
        .prop_map(|(f, o, l)| Expr::cmp(f, o, l))
}

fn query() -> impl Strategy<Value = Expr> {
    comparison().prop_recursive(3, 8, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::and(a, b)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Expr::or(a, b)),
            inner.prop_map(Expr::not),
        ]
    })
}

fn brute(all: &[TelemetryEvent], e: &Expr, range: TimeRange) -> Vec<(i64, u64)> {
    let c = compile(e, Signal::Logs);
    let mut keys: Vec<(i64, u64)> = all
        .iter()
        .filter(|ev| range.contains(ev.timestamp) && eval(&c, *ev))
        .map(|ev| (ev.timestamp.0, ev.id.0))
        .collect();
    keys.sort();
    keys
}

fn range_strategy() -> impl Strategy<Value = TimeRange> {
    prop_oneof![
        Just(TimeRange::all()),
        (0i64..300, 1i64..300).prop_map(|(a, len)| TimeRange::new(
            Some(Timestamp(BASE + a * 1_000_000_000)),
            Some(Timestamp(BASE + (a + len) * 1_000_000_000))
        )),
    ]
}

#[test]
fn indexed_count_and_search_equal_brute_force() {
    let fx = fixture(7, 400);
    let mut runner = TestRunner::new(Config { cases: 400, ..Config::default() });
    let nonempty = std::cell::Cell::new(0usize);
    let pruned = std::cell::Cell::new(0usize);
    runner
        .run(&(query(), range_strategy()), |(e, range)| {
            let text = e.to_string();
            let expected = brute(&fx.all, &e, range);
            if !expected.is_empty() {
                nonempty.set(nonempty.get() + 1);
            }

            let (count, _) = fx.engine.count(Signal::Logs, &text, range).unwrap();
            prop_assert_eq!(count as usize, expected.len(), "count mismatch for {}", text);

            // Full result set via search (limit larger than possible).
            let out = fx.engine.search(Signal::Logs, &text, range, 1000, Direction::Forward, None).unwrap();
            let got: Vec<(i64, u64)> = out.events.iter().map(|ev| (ev.timestamp.0, ev.id.0)).collect();
            prop_assert_eq!(&got, &expected, "search mismatch for {}", text);
            if out.diagnostics.blocks_skipped > 0 {
                pruned.set(pruned.get() + 1);
            }
            Ok(())
        })
        .unwrap();
    // Guard against a vacuous test: plenty of queries must match something
    // and plenty must have been answered with the help of indexes.
    assert!(nonempty.get() > 100, "only {} non-empty cases", nonempty.get());
    assert!(pruned.get() > 50, "only {} cases used pruning", pruned.get());
}

#[test]
fn pagination_walks_the_exact_result_set() {
    let fx = fixture(11, 300);
    let mut runner = TestRunner::new(Config { cases: 60, ..Config::default() });
    runner
        .run(&(query(), 1usize..40, any::<bool>()), |(e, page, backward)| {
            let text = e.to_string();
            let mut expected = brute(&fx.all, &e, TimeRange::all());
            let dir = if backward {
                expected.reverse();
                Direction::Backward
            } else {
                Direction::Forward
            };
            let mut got = Vec::new();
            let mut cursor = None;
            for _ in 0..1000 {
                let out = fx.engine.search(Signal::Logs, &text, TimeRange::all(), page, dir, cursor).unwrap();
                prop_assert!(out.events.len() <= page);
                got.extend(out.events.iter().map(|ev| (ev.timestamp.0, ev.id.0)));
                cursor = out.next;
                if cursor.is_none() {
                    break;
                }
            }
            prop_assert_eq!(&got, &expected, "pagination mismatch for {}", text);
            Ok(())
        })
        .unwrap();
}

#[test]
fn histogram_totals_match_count() {
    let fx = fixture(3, 200);
    let range = TimeRange::new(Some(Timestamp(BASE - 10_000_000_000)), Some(Timestamp(BASE + 400_000_000_000)));
    for q in ["", "level = Error", r#"customerId = 5 or service = "auth""#, "not durationMs > 100"] {
        let (h, _) = fx.engine.histogram(Signal::Logs, q, range, 50).unwrap();
        let (c, _) = fx.engine.count(Signal::Logs, q, range).unwrap();
        assert_eq!(h.total, c, "{q}");
        assert_eq!(h.buckets.iter().map(|b| b.count).sum::<u64>(), c);
        if q.is_empty() {
            assert_eq!(c as usize, fx.all.len());
        }
    }
}

#[test]
fn indexes_actually_skip_work() {
    let fx = fixture(5, 400);
    let id = fx.all.iter().find_map(|e| e.trace_id.clone()).unwrap();
    let out = fx
        .engine
        .search(Signal::Logs, &format!("traceId = \"{id}\""), TimeRange::all(), 100, Direction::Backward, None)
        .unwrap();
    let d = &out.diagnostics;
    assert!(!out.events.is_empty());
    assert!(d.blocks_skipped > 0, "{d:?}");
    assert!(d.indexes.iter().any(|i| i.field == "traceId" && i.kind == "bloom"));

    let out = fx
        .engine
        .search(Signal::Logs, r#"service = "nope""#, TimeRange::all(), 100, Direction::Backward, None)
        .unwrap();
    assert!(out.events.is_empty());
    assert_eq!(out.diagnostics.blocks_read, 0);
    assert_eq!(out.diagnostics.segments_skipped_by_index, out.diagnostics.segments_considered);
}

#[test]
fn early_termination_reads_only_recent_segments() {
    let fx = fixture(9, 400);
    let out = fx.engine.search(Signal::Logs, "", TimeRange::all(), 5, Direction::Backward, None).unwrap();
    assert_eq!(out.events.len(), 5);
    let mut expected: Vec<_> = fx.all.iter().map(|e| (e.timestamp.0, e.id.0)).collect();
    expected.sort();
    expected.reverse();
    let got: Vec<_> = out.events.iter().map(|e| (e.timestamp.0, e.id.0)).collect();
    assert_eq!(got, expected[..5]);
    assert!(out.next.is_some());
}
