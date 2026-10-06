//! Query benchmarks: parsing, a full-scan query and index-assisted queries
//! over a multi-segment store. Run with `cargo bench -p query`.

use std::hint::black_box;
use std::sync::{Arc, mpsc};

use criterion::{Criterion, criterion_group, criterion_main};
use query::{Direction, Engine, QueryLimits, TimeRange, parse};
use storage::{Storage, StorageConfig};
use telemetry::*;

fn event(i: usize) -> TelemetryEvent {
    let mut attrs = Fields::new();
    attrs.insert("customerId", Value::Int((i % 10_000) as i64));
    attrs.insert("durationMs", Value::Int((i % 2000) as i64));
    TelemetryEvent {
        id: EventId(0),
        timestamp: Timestamp(1_800_000_000_000_000_000 + i as i64 * 1_000_000),
        observed_timestamp: Timestamp(0),
        service: Some(["payments", "orders", "auth", "search"][i % 4].into()),
        environment: Some("production".into()),
        trace_id: TraceId::new(&format!("{:032x}", i + 1)),
        span_id: None,
        resource: Fields::new(),
        attributes: attrs,
        payload: TelemetryPayload::Log(LogEvent {
            level: if i.is_multiple_of(100) { Level::Error } else { Level::Information },
            message: format!("request {i} handled"),
            message_template: None,
            exception: None,
        }),
    }
}

fn setup() -> (tempfile::TempDir, Engine) {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = StorageConfig::new(d.path());
    cfg.compaction.enabled = false;
    let storage = Arc::new(Storage::open(cfg, None).unwrap());
    let per_segment = 50_000;
    for s in 0..4 {
        for chunk in (0..per_segment).collect::<Vec<_>>().chunks(5_000) {
            let evs: Vec<_> = chunk.iter().map(|i| event(s * per_segment + i)).collect();
            let (tx, rx) = mpsc::sync_channel(1);
            storage.submit(Signal::Logs, evs, Box::new(move |r| tx.send(r).unwrap())).unwrap();
            rx.recv().unwrap().unwrap();
        }
        storage.rotate_blocking(Signal::Logs).unwrap();
    }
    (d, Engine::new(storage, QueryLimits::default()))
}

fn benches(c: &mut Criterion) {
    c.bench_function("parse_query", |b| {
        b.iter(|| parse(black_box(r#"level = "Error" and (service = "payments" or http.statusCode >= 500) and not message contains "health""#)).unwrap())
    });

    let (_d, engine) = setup();
    let mut g = c.benchmark_group("query_200k_events");
    g.sample_size(20);
    let engine = &engine;
    let run = move |q: &'static str, limit: usize| {
        move || engine.search(Signal::Logs, q, TimeRange::all(), limit, Direction::Backward, None).unwrap()
    };
    g.bench_function("simple_scan_contains", |b| b.iter(run(r#"message contains "request 1999""#, 100)));
    g.bench_function("indexed_bloom_trace_id", |b| b.iter(run(r#"traceId = "00000000000000000000000000012345""#, 100)));
    g.bench_function("indexed_bitmap_level", |b| b.iter(run("level = Error", 100)));
    g.bench_function("indexed_zonemap_range", |b| b.iter(run("durationMs > 1990", 100)));
    g.bench_function("latest_200", |b| b.iter(run("", 200)));
    g.bench_function("count_all", |b| b.iter(|| engine.count(Signal::Logs, "", TimeRange::all()).unwrap()));
    g.finish();
}

criterion_group!(group, benches);
criterion_main!(group);
