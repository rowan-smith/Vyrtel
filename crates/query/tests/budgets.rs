//! Execution budgets and cancellation (REL-12) at the engine level.
//!
//! Fixture: 2,000 logs, 300 spans (30 traces of 10 spans) and 600 metric
//! points (two each of 300 names, `m-000` to `m-299`) with 16-event blocks,
//! sealed into three segments per signal plus an unsealed tail, so every
//! query crosses segments, blocks and the unsealed batches. Event `i` is at
//! `BASE + i s`; every seventh log is an `Error`.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use query::aggregate::{Agg, CountSink};
use query::budget::ROWS_PER_CHECK;
use query::eval::Row;
use query::exec::{self, ExecCtx, Sink};
use query::{
    Budget, CancelToken, Checkpoint, CheckpointHook, Direction, Engine, MetricQuery, QueryError, QueryLimits, TimeRange,
};
use storage::segment::SegmentWriteOptions;
use storage::{Storage, StorageConfig};
use telemetry::*;

const BASE: i64 = 1_800_000_000_000_000_000;
const LOGS: usize = 2_000;
const ERRORS: u64 = LOGS.div_ceil(7) as u64;
const TRACES: usize = 30;
const SPANS_PER_TRACE: usize = 10;
const METRIC_NAMES: usize = 300;

fn log(i: usize) -> TelemetryEvent {
    TelemetryEvent {
        id: EventId(0),
        timestamp: Timestamp(BASE + i as i64 * NANOS_PER_SEC),
        observed_timestamp: Timestamp(BASE),
        service: Some(["payments", "orders"][i % 2].into()),
        environment: None,
        trace_id: None,
        span_id: None,
        resource: Fields::new().into(),
        attributes: Fields::new(),
        payload: TelemetryPayload::Log(LogEvent {
            level: if i.is_multiple_of(7) { Level::Error } else { Level::Information },
            message: format!("request {i}"),
            message_template: None,
            exception: None,
        }),
    }
}

fn trace_id(t: usize) -> String {
    format!("{:032x}", t + 1)
}

fn span(t: usize, s: usize) -> TelemetryEvent {
    TelemetryEvent {
        id: EventId(0),
        timestamp: Timestamp(BASE + (t * 100 + s) as i64 * NANOS_PER_SEC),
        observed_timestamp: Timestamp(BASE),
        service: Some("api".into()),
        environment: None,
        trace_id: TraceId::new(&trace_id(t)),
        span_id: SpanId::new(&format!("{:016x}", t * 100 + s + 1)),
        resource: Fields::new().into(),
        attributes: Fields::new(),
        payload: TelemetryPayload::Span(SpanEvent {
            parent_span_id: (s > 0).then(|| SpanId::new(&format!("{:016x}", t * 100 + 1)).unwrap()),
            name: format!("op-{s}"),
            kind: SpanKind::Server,
            duration_nanos: 1_000,
            status: SpanStatus { code: StatusCode::Ok, message: None },
            events: vec![],
            links: vec![],
        }),
    }
}

fn point(i: usize) -> TelemetryEvent {
    TelemetryEvent {
        id: EventId(0),
        timestamp: Timestamp(BASE + i as i64 * NANOS_PER_SEC),
        observed_timestamp: Timestamp(BASE),
        service: Some("api".into()),
        environment: None,
        trace_id: None,
        span_id: None,
        resource: Fields::new().into(),
        attributes: Fields::new(),
        payload: TelemetryPayload::Metric(MetricEvent {
            name: metric_name(i % METRIC_NAMES),
            description: None,
            unit: None,
            kind: MetricKind::Gauge,
            temporality: Temporality::Unspecified,
            monotonic: false,
            value: MetricValue::Number(i as f64),
        }),
    }
}

fn metric_name(n: usize) -> String {
    format!("m-{n:03}")
}

struct Fixture {
    _dir: tempfile::TempDir,
    storage: Arc<Storage>,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = StorageConfig::new(dir.path());
        cfg.maintenance_interval = Duration::from_secs(3600);
        cfg.compaction.enabled = false;
        cfg.segment = SegmentWriteOptions { block_events: 16, ..Default::default() };
        let storage = Arc::new(Storage::open(cfg, None).unwrap());
        let logs: Vec<_> = (0..LOGS).map(log).collect();
        let spans: Vec<_> = (0..TRACES).flat_map(|t| (0..SPANS_PER_TRACE).map(move |s| span(t, s))).collect();
        let points: Vec<_> = (0..2 * METRIC_NAMES).map(point).collect();
        for (signal, events) in [(Signal::Logs, logs), (Signal::Traces, spans), (Signal::Metrics, points)] {
            // Three segments, then an unsealed tail.
            for (k, chunk) in events.chunks(events.len() / 4 + 1).enumerate() {
                let (tx, rx) = mpsc::sync_channel(1);
                storage.submit(signal, chunk.to_vec(), Box::new(move |r| tx.send(r).unwrap())).unwrap();
                rx.recv().unwrap().unwrap();
                if k < 3 {
                    storage.rotate_blocking(signal).unwrap();
                }
            }
        }
        Fixture { _dir: dir, storage }
    }

    fn engine(&self, limits: QueryLimits) -> Engine {
        Engine::new(self.storage.clone(), limits)
    }
}

fn all() -> TimeRange {
    TimeRange::all()
}

fn bounded() -> TimeRange {
    TimeRange::new(Some(Timestamp(BASE)), Some(Timestamp(BASE + 10_000 * NANOS_PER_SEC)))
}

fn metric(range: TimeRange, step_ms: Option<i64>) -> MetricQuery {
    MetricQuery { name: "cpu".into(), filter: None, range, step_ms, agg: Agg::Avg, group_by: None }
}

/// Runs every kind of request with `cancel`, returning each outcome.
fn every_request(e: &Engine, cancel: &CancelToken) -> Vec<(&'static str, Result<(), QueryError>)> {
    let t = trace_id(3);
    vec![
        ("search", e.search(Signal::Logs, "", all(), 100, Direction::Backward, None, cancel).map(drop)),
        ("count", e.count(Signal::Logs, "level = Error", all(), cancel).map(drop)),
        ("histogram", e.histogram(Signal::Logs, "", bounded(), 60, cancel).map(drop)),
        ("facets", e.facets(Signal::Logs, "", all(), 500, cancel).map(drop)),
        ("traceSearch", e.trace_search("", all(), 10, cancel).map(drop)),
        ("trace", e.trace(&t, all(), cancel).map(drop)),
        ("metric", e.metric_query(&metric(bounded(), None), cancel).map(drop)),
        ("metricNames", e.metric_names(cancel).map(drop)),
    ]
}

/// One request run with the given cancellation token.
type Request<'a> = dyn Fn(&CancelToken) -> Result<(), QueryError> + 'a;

/// Records every checkpoint; cancels at the `n`th one of kind `at`.
fn cancel_at(at: Checkpoint, n: usize, seen: Arc<Mutex<Vec<Checkpoint>>>) -> CheckpointHook {
    let count = AtomicUsize::new(0);
    Arc::new(move |c, token| {
        seen.lock().unwrap().push(c);
        if c == at && count.fetch_add(1, Ordering::SeqCst) + 1 == n {
            token.cancel();
        }
    })
}

// AC2/AC4: a cancelled token stops every kind of request before it reads
// anything, and none of them returns data.
#[test]
fn cancelled_requests_return_only_the_error() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let cancel = CancelToken::new();
    cancel.cancel();
    for (what, r) in every_request(&e, &cancel) {
        assert!(matches!(r, Err(QueryError::Cancelled)), "{what}: {r:?}");
    }
}

// TC-02, scan stage: cancelling between block reads stops the search at
// the next block checkpoint; it reads no further block.
#[test]
fn cancel_during_scan_stops_at_the_next_block() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let full = e.search(Signal::Logs, "level = Error", all(), 1000, Direction::Backward, None, &CancelToken::new());
    assert!(full.unwrap().diagnostics.blocks_read > 4, "the fixture needs several block reads");

    let seen = Arc::new(Mutex::new(Vec::new()));
    e.set_checkpoint_hook(Some(cancel_at(Checkpoint::Block, 3, seen.clone())));
    let r =
        e.search(Signal::Logs, "level = Error", all(), 1000, Direction::Backward, None, &CancelToken::new()).map(drop);
    assert!(matches!(r, Err(QueryError::Cancelled)), "{r:?}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.iter().filter(|c| **c == Checkpoint::Block).count(), 3, "stopped at the cancelling checkpoint");
    assert_eq!(seen.last(), Some(&Checkpoint::Block));
}

/// Counts rows and cancels the request at row `at`.
struct CancellingSink {
    rows: usize,
    at: usize,
    cancel: CancelToken,
}

impl Sink for CancellingSink {
    fn row(&mut self, _row: &dyn Row) -> ControlFlow<()> {
        self.rows += 1;
        if self.rows == self.at {
            self.cancel.cancel();
        }
        ControlFlow::Continue(())
    }
}

// TC-02, aggregation stage: an aggregation cancelled while it consumes
// rows (sealed or unsealed) stops within one row checkpoint interval.
#[test]
fn cancel_during_aggregation_stops_within_one_check_interval() {
    let fx = Fixture::new();
    let limits = QueryLimits::default();
    let snapshot = fx.storage.snapshot(Signal::Logs);
    // Unsealed batches are scanned first: cancel there, then deep in the
    // sealed segments.
    for at in [10, LOGS / 2] {
        let cancel = CancelToken::new();
        let budget = Budget::new(&limits, cancel.clone());
        let mut ctx = ExecCtx::new(fx.storage.cache(), &budget, limits.max_results);
        let mut sink = CancellingSink { rows: 0, at, cancel };
        let r = exec::scan(&snapshot, Signal::Logs, None, all(), &mut sink, &mut ctx);
        assert!(matches!(r, Err(QueryError::Cancelled)), "at {at}: {r:?}");
        let after = sink.rows - at;
        assert!(after < ROWS_PER_CHECK as usize, "{after} rows consumed after cancelling at {at}");
    }
}

// The engine's aggregations hit the same checkpoints.
#[test]
fn cancel_at_a_row_checkpoint_stops_every_aggregation() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let requests: [(&str, &Request); 3] = [
        ("count", &|c| e.count(Signal::Logs, "level = Error", all(), c).map(drop)),
        ("histogram", &|c| e.histogram(Signal::Logs, "level = Error", bounded(), 60, c).map(drop)),
        ("facets", &|c| e.facets(Signal::Logs, "", all(), 1000, c).map(drop)),
    ];
    for (what, run) in requests {
        e.set_checkpoint_hook(Some(cancel_at(Checkpoint::Rows, 1, Arc::new(Mutex::new(Vec::new())))));
        let r = run(&CancelToken::new());
        assert!(matches!(r, Err(QueryError::Cancelled)), "{what}: {r:?}");
    }
}

// Trace search runs up to 20 pages and an assembly scan. Cancelling at
// the assembly checkpoint discards the spans already found.
#[test]
fn cancel_between_trace_search_phases() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    e.set_checkpoint_hook(Some(cancel_at(Checkpoint::Assemble, 1, seen)));
    let r = e.trace_search("", all(), 10, &CancelToken::new());
    assert!(matches!(r, Err(QueryError::Cancelled)), "{r:?}");
}

// AC1/TC-02: the scan-byte budget allows exactly `max_scan_bytes` and
// fails one byte below what the request needs, including requests made of
// several scans (facets, trace search pages + assembly).
#[test]
fn scan_byte_budget_boundary() {
    let fx = Fixture::new();
    let open = fx.engine(QueryLimits::default());
    let c = CancelToken::new();
    let needed = [
        ("count", open.count(Signal::Logs, "level = Error", all(), &c).unwrap().1.bytes_read),
        ("facets", open.facets(Signal::Logs, "", all(), 1000, &c).unwrap().1.bytes_read),
        ("traceSearch", open.trace_search("", all(), 30, &c).unwrap().1.bytes_read),
    ];
    for (what, bytes) in needed {
        assert!(bytes > 0, "{what} reads segments");
        let run = |max_scan_bytes| {
            let e = fx.engine(QueryLimits { max_scan_bytes, ..QueryLimits::default() });
            match what {
                "count" => e.count(Signal::Logs, "level = Error", all(), &c).map(|r| r.1.bytes_read),
                "facets" => e.facets(Signal::Logs, "", all(), 1000, &c).map(|r| r.1.bytes_read),
                _ => e.trace_search("", all(), 30, &c).map(|r| r.1.bytes_read),
            }
        };
        assert_eq!(run(bytes).unwrap(), bytes, "{what}: exactly the budget succeeds");
        let r = run(bytes - 1);
        assert!(matches!(r, Err(QueryError::TooExpensive(b)) if b == bytes - 1), "{what}: {r:?}");
    }
}

// AC1/TC-03: a request over its runtime budget fails with Timeout and
// returns nothing; the next request on the same engine is complete.
#[test]
fn timeout_then_a_small_query_is_complete() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits { timeout: Duration::from_millis(20), ..QueryLimits::default() });
    let slow = Arc::new(AtomicBool::new(true));
    let s = slow.clone();
    e.set_checkpoint_hook(Some(Arc::new(move |at, _| {
        if at == Checkpoint::Start && s.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(40));
        }
    })));
    for (what, r) in every_request(&e, &CancelToken::new()) {
        assert!(matches!(r, Err(QueryError::Timeout(20))), "{what}: {r:?}");
    }

    slow.store(false, Ordering::SeqCst);
    let (n, _) = e.count(Signal::Logs, "level = Error", all(), &CancelToken::new()).unwrap();
    assert_eq!(n, ERRORS);
    let out = e.search(Signal::Logs, "", all(), 5, Direction::Backward, None, &CancelToken::new()).unwrap();
    let got: Vec<_> = out.events.iter().map(|ev| ev.message().to_string()).collect();
    assert_eq!(got, ["request 1999", "request 1998", "request 1997", "request 1996", "request 1995"]);
    assert!(out.next.is_some());
}

// TC-03 after cancellation and the byte budget: rejected requests leave
// nothing behind that changes later answers.
#[test]
fn later_queries_are_complete_after_cancellation_and_limits() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(e.count(Signal::Logs, "level = Error", all(), &cancelled).is_err());
    let tight = fx.engine(QueryLimits { max_scan_bytes: 1, ..QueryLimits::default() });
    assert!(tight.count(Signal::Logs, "level = Error", all(), &CancelToken::new()).is_err());

    let (n, _) = e.count(Signal::Logs, "level = Error", all(), &CancelToken::new()).unwrap();
    assert_eq!(n, ERRORS);
    let (traces, _) = e.trace_search("", all(), 50, &CancelToken::new()).unwrap();
    assert_eq!(traces.len(), TRACES);
    assert!(traces.iter().all(|t| t.span_count == SPANS_PER_TRACE && !t.truncated));
}

// AC1/TC-02: result budgets. A trace with more spans than
// `max_trace_spans` is refused rather than returned incomplete; exactly
// the limit is returned in full.
#[test]
fn trace_span_budget_boundary() {
    let fx = Fixture::new();
    let c = CancelToken::new();
    let id = trace_id(7);
    let at = |max_trace_spans| fx.engine(QueryLimits { max_trace_spans, ..QueryLimits::default() });
    let (spans, _) = at(SPANS_PER_TRACE).trace(&id, all(), &c).unwrap();
    assert_eq!(spans.len(), SPANS_PER_TRACE);
    let r = at(SPANS_PER_TRACE - 1).trace(&id, all(), &c);
    assert!(matches!(r, Err(QueryError::ResultLimit { limit, .. }) if limit == SPANS_PER_TRACE as u64 - 1), "{r:?}");
}

// Metric series are refused when they would need more points than fit,
// instead of silently stopping at the last bucket.
#[test]
fn metric_point_budget_boundary() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let c = CancelToken::new();
    let max = query::MAX_METRIC_POINTS as i64;
    let range = |points: i64| TimeRange::new(Some(Timestamp(BASE)), Some(Timestamp(BASE + points * NANOS_PER_SEC)));
    e.metric_query(&metric(range(max), Some(1000)), &c).expect("exactly the limit");
    let r = e.metric_query(&metric(range(max + 1), Some(1000)), &c);
    assert!(matches!(r, Err(QueryError::ResultLimit { limit, .. }) if limit == max as u64), "{r:?}");
}

// Unfiltered counts use block summaries without reading; they still stop
// at segment checkpoints.
#[test]
fn summary_only_counts_still_check_segments() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let (n, d) = e.count(Signal::Logs, "", all(), &CancelToken::new()).unwrap();
    assert_eq!((n, d.bytes_read), (LOGS as u64, 0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    e.set_checkpoint_hook(Some(cancel_at(Checkpoint::Segment, 1, seen)));
    let r = e.count(Signal::Logs, "", all(), &CancelToken::new());
    assert!(matches!(r, Err(QueryError::Cancelled)), "{r:?}");
}

// Removing the hook restores normal execution, and a sink-driven scan
// over everything sees each event once.
#[test]
fn hook_can_be_removed() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    e.set_checkpoint_hook(Some(Arc::new(|_, t| t.cancel())));
    assert!(e.count(Signal::Logs, "", all(), &CancelToken::new()).is_err());
    e.set_checkpoint_hook(None);
    let limits = QueryLimits::default();
    let budget = Budget::new(&limits, CancelToken::new());
    let mut ctx = ExecCtx::new(fx.storage.cache(), &budget, limits.max_results);
    let mut sink = CountSink { count: 0 };
    exec::scan(&fx.storage.snapshot(Signal::Logs), Signal::Logs, None, all(), &mut sink, &mut ctx).unwrap();
    assert_eq!(sink.count, LOGS as u64);
}

// Review finding 1: metric-name discovery runs within the request budget.
// Cancelled while it collects names, it stops at the next check and
// returns only the error; the next discovery is complete.
#[test]
fn metric_name_discovery_stops_when_cancelled_during_enumeration() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    e.set_checkpoint_hook(Some(cancel_at(Checkpoint::Aggregate, 1, seen.clone())));
    let r = e.metric_names(&CancelToken::new());
    assert!(matches!(r, Err(QueryError::Cancelled)), "{r:?}");
    {
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&Checkpoint::Segment), "checked per segment: {seen:?}");
        assert_eq!(seen.last(), Some(&Checkpoint::Aggregate), "stopped while collecting names");
        assert!(!seen.contains(&Checkpoint::Assemble), "never reached the end");
    }

    e.set_checkpoint_hook(None);
    let names = e.metric_names(&CancelToken::new()).unwrap();
    let expected: Vec<String> = (0..METRIC_NAMES).map(metric_name).collect();
    assert_eq!(names.metrics.iter().map(|m| m.name.clone()).collect::<Vec<_>>(), expected);
    assert!(!names.truncated);
}

// Discovery returns at most `max_metric_names`, the first by name, and
// says when more exist. Exactly the limit is complete.
#[test]
fn metric_name_budget_boundary() {
    let fx = Fixture::new();
    let c = CancelToken::new();
    for (max, truncated) in [(METRIC_NAMES + 1, false), (METRIC_NAMES, false), (METRIC_NAMES - 1, true), (1, true)] {
        let e = fx.engine(QueryLimits { max_metric_names: max, ..QueryLimits::default() });
        let names = e.metric_names(&c).unwrap();
        let expected: Vec<String> = (0..max.min(METRIC_NAMES)).map(metric_name).collect();
        assert_eq!(names.metrics.iter().map(|m| m.name.clone()).collect::<Vec<_>>(), expected, "max {max}");
        assert_eq!(names.truncated, truncated, "max {max}");
    }
}

// Review finding 2: facet counting after the sample scan checks the budget
// too. Cancelled there, facets return only the error.
#[test]
fn facet_counting_after_the_scan_checks_the_budget() {
    let fx = Fixture::new();
    let e = fx.engine(QueryLimits::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    e.set_checkpoint_hook(Some(cancel_at(Checkpoint::Aggregate, 1, seen.clone())));
    let r = e.facets(Signal::Logs, "", all(), 1000, &CancelToken::new());
    assert!(matches!(r, Err(QueryError::Cancelled)), "{:?}", r.map(drop));
    let seen = seen.lock().unwrap();
    let assemble = seen.iter().position(|c| *c == Checkpoint::Assemble).expect("the scan finished");
    assert!(seen[assemble..].contains(&Checkpoint::Aggregate), "cancelled after the scan: {seen:?}");
}

// A single deeply structured event is enough to reach a check: property
// leaves count, not only events.
#[test]
fn facet_counting_checks_within_one_event() {
    let mut attributes = Fields::new();
    for i in 0..2 * ROWS_PER_CHECK {
        attributes.insert(format!("p{i}"), Value::Int(i as i64));
    }
    let event = TelemetryEvent { attributes, ..log(1) };
    let limits = QueryLimits::default();
    let cancel = CancelToken::new();
    let f = query::aggregate::facets(std::slice::from_ref(&event), 10, &Budget::new(&limits, cancel.clone())).unwrap();
    assert_eq!(f.sampled, 1);
    cancel.cancel();
    let r = query::aggregate::facets(std::slice::from_ref(&event), 10, &Budget::new(&limits, cancel));
    assert!(matches!(r, Err(QueryError::Cancelled)), "{:?}", r.map(drop));
}
