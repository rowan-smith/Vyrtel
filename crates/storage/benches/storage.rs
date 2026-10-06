//! Storage primitives: WAL append, segment creation/decoding, Zstd blocks.
//!
//! Run with `cargo bench -p storage`. Results depend heavily on hardware
//! and event shape; see docs/testing.md for how to read them.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use storage::codec;
use storage::segment::{ColumnSet, Segment, SegmentWriteOptions, write_segment};
use storage::wal::{WalHeader, WalWriter};
use telemetry::*;

fn events(n: usize) -> Vec<TelemetryEvent> {
    let services = ["payments", "orders", "auth", "inventory"];
    (0..n)
        .map(|i| {
            let mut attrs = Fields::new();
            attrs.insert("customerId", Value::Int((i % 5000) as i64));
            attrs.insert("requestId", Value::from(format!("req-{i:08x}")));
            attrs.insert("durationMs", Value::Float((i % 997) as f64 * 1.3));
            let mut http = Fields::new();
            http.insert("method", Value::from(if i % 3 == 0 { "POST" } else { "GET" }));
            http.insert("statusCode", Value::Int(if i % 50 == 0 { 500 } else { 200 }));
            attrs.insert("http", Value::Object(http));
            TelemetryEvent {
                id: EventId(i as u64 + 1),
                timestamp: Timestamp(1_800_000_000_000_000_000 + i as i64 * 1_000_000),
                observed_timestamp: Timestamp(1_800_000_000_000_000_000 + i as i64 * 1_000_000 + 500),
                service: Some(services[i % services.len()].to_string()),
                environment: Some("production".into()),
                trace_id: TraceId::new(&format!("{:032x}", i / 4 + 1)),
                span_id: SpanId::new(&format!("{:016x}", i + 1)),
                resource: [("host.name".to_string(), Value::from(format!("web-{}", i % 8)))].into_iter().collect(),
                attributes: attrs,
                payload: TelemetryPayload::Log(LogEvent {
                    level: if i % 50 == 0 { Level::Error } else { Level::Information },
                    message: format!("Handled request {i} for customer {}", i % 5000),
                    message_template: Some("Handled request {RequestId} for customer {CustomerId}".into()),
                    exception: None,
                }),
            }
        })
        .collect()
}

fn wal_append(c: &mut Criterion) {
    let mut g = c.benchmark_group("wal_append");
    for batch in [1usize, 100] {
        let evs = events(batch);
        let bytes = codec::encode_batch(&evs);
        g.throughput(Throughput::Elements(batch as u64));
        g.bench_with_input(BenchmarkId::from_parameter(batch), &bytes, |b, bytes| {
            let d = tempfile::tempdir().unwrap();
            let mut w = WalWriter::create(d.path(), d.path(), WalHeader { signal: Signal::Logs, wal_id: 1 }).unwrap();
            b.iter(|| w.append(black_box(bytes)).unwrap());
        });
    }
    g.finish();
}

fn encode_batch(c: &mut Criterion) {
    let evs = events(1000);
    let mut g = c.benchmark_group("codec");
    g.throughput(Throughput::Elements(1000));
    g.bench_function("encode_batch_1000", |b| b.iter(|| codec::encode_batch(black_box(&evs))));
    let bytes = codec::encode_batch(&evs);
    g.bench_function("decode_batch_1000", |b| b.iter(|| codec::decode_batch(black_box(&bytes)).unwrap()));
    g.finish();
}

fn segments(c: &mut Criterion) {
    let evs = events(20_000);
    let refs: Vec<&TelemetryEvent> = evs.iter().collect();
    let opts = SegmentWriteOptions::default();
    let mut g = c.benchmark_group("segment");
    g.sample_size(10);
    g.throughput(Throughput::Elements(evs.len() as u64));
    g.bench_function("create_20k", |b| {
        let d = tempfile::tempdir().unwrap();
        let mut id = 0;
        b.iter(|| {
            id += 1;
            write_segment(d.path(), d.path(), id, Signal::Logs, &refs, vec![], &opts).unwrap()
        });
    });

    let d = tempfile::tempdir().unwrap();
    let w = write_segment(d.path(), d.path(), 1, Signal::Logs, &refs, vec![], &opts).unwrap();
    let seg = Segment::open(&w.path).unwrap();
    eprintln!(
        "segment: {} events, raw {} B, data {} B, index {} B, ratio {:.2}x",
        seg.summary.event_count,
        seg.summary.raw_bytes,
        seg.summary.data_bytes,
        seg.summary.index_bytes,
        seg.summary.raw_bytes as f64 / (seg.summary.data_bytes + seg.summary.index_bytes) as f64
    );
    g.bench_function("decode_all_20k", |b| b.iter(|| seg.read_all().unwrap()));
    g.finish();

    let mut g = c.benchmark_group("zstd_block");
    let block = seg.summary.blocks.len() / 2;
    g.throughput(Throughput::Bytes(seg.summary.blocks[block].raw_bytes()));
    g.bench_function("decompress_all_columns", |b| b.iter(|| seg.read_block(block, ColumnSet::ALL).unwrap()));
    g.bench_function("decompress_meta_only", |b| {
        b.iter(|| seg.read_block(block, ColumnSet::new().with(storage::segment::Column::Meta)).unwrap())
    });
    g.finish();
}

criterion_group!(benches, wal_append, encode_batch, segments);
criterion_main!(benches);
