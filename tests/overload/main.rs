//! Memory under sustained ingest overload (REL-02).
//!
//! A separate test binary because it installs a counting global allocator
//! and measures the whole process: nothing else may run alongside it, and
//! the tests here take [`SERIAL`] so they do not overlap either.
//! The figures are printed; run with `--nocapture` to record them.

#[path = "../integration/common/mod.rs"]
mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::*;
use server::config::ByteSize;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                let now = CURRENT.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size - layout.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Start a measurement window; returns the heap size at its start.
fn reset_peak() -> usize {
    let now = CURRENT.load(Ordering::Relaxed);
    PEAK.store(now, Ordering::Relaxed);
    now
}

const MB: f64 = (1 << 20) as f64;

/// Heap measurements are process-wide: one test at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// Fixed limits: 64MB memory budget (the minimum), 2MB requests, 2 ingest
// slots, 32 clients each sending ~1.8MB batches as fast as they are
// admitted, half of them gzip-compressed. Peak heap growth over the run
// must stay within `storage.max_memory`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_overload_stays_within_memory_budget() {
    let _serial = SERIAL.lock().await;
    const CLIENTS: usize = 32;
    const RUN: Duration = Duration::from_secs(4);
    let s = TestServer::start_with(|c| {
        c.storage.max_memory = ByteSize(64 << 20);
        c.ingest.max_request_size = ByteSize(2 << 20);
        c.ingest.max_concurrent = 2;
        c.alerts.enabled = false;
    })
    .await;
    let budget = s.config.storage.max_memory.0 as usize;

    let plain = bytes_of(ndjson_payload(8_000, 160));
    let compressed = bytes_of(gzip(&plain));
    assert!(plain.len() < 2 << 20);

    // Decoding alone, then one request end to end (body, decoded events,
    // WAL record and the copy kept in the active segment).
    let base = reset_peak();
    let events = ingest::native::parse(&plain, ingest::native::Format::NdJson, telemetry::Timestamp::now()).unwrap();
    let parse_peak = PEAK.load(Ordering::Relaxed) - base;
    drop(events);
    let base = reset_peak();
    let r = post(&s, &plain, false).await;
    assert_eq!(r, 200);
    let single = PEAK.load(Ordering::Relaxed) - base;

    let base = reset_peak();
    let accepted = Arc::new(AtomicU64::new(0));
    let rejected = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let slowest_rejection = Arc::new(AtomicU64::new(0));
    let deadline = Instant::now() + RUN;
    let mut tasks = Vec::new();
    for i in 0..CLIENTS {
        let (client, url) = (s.client.clone(), s.u("/api/v1/events"));
        let body = if i % 2 == 0 { plain.clone() } else { compressed.clone() };
        let (accepted, rejected, errors, slowest) =
            (accepted.clone(), rejected.clone(), errors.clone(), slowest_rejection.clone());
        tasks.push(tokio::spawn(async move {
            while Instant::now() < deadline {
                let mut req = client.post(&url).header("content-type", "application/x-ndjson");
                if i % 2 == 1 {
                    req = req.header("content-encoding", "gzip");
                }
                let started = Instant::now();
                let status = match req.body(body.clone()).send().await {
                    Ok(r) => r.status().as_u16(),
                    Err(e) => {
                        eprintln!("client {i}: transport error after {:?}: {e}", started.elapsed());
                        errors.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                match status {
                    200 => {
                        accepted.fetch_add(1, Ordering::Relaxed);
                    }
                    429 => {
                        rejected.fetch_add(1, Ordering::Relaxed);
                        slowest.fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    other => panic!("unexpected status {other}"),
                }
            }
        }));
    }
    let mut max_in_flight = 0;
    while Instant::now() < deadline {
        max_in_flight = max_in_flight.max(ingest_in_flight(&s));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    for t in tasks {
        t.await.unwrap();
    }
    let peak = PEAK.load(Ordering::Relaxed) - base;
    let (accepted, rejected) = (accepted.load(Ordering::Relaxed), rejected.load(Ordering::Relaxed));
    let errors = errors.load(Ordering::Relaxed);
    let slowest = Duration::from_micros(slowest_rejection.load(Ordering::Relaxed));

    eprintln!(
        "REL-02 overload: body {:.2} MB ({:.2} MB gzip); parse peak +{:.1} MB; single request peak +{:.1} MB; \
         {CLIENTS} clients for {RUN:?}: {accepted} accepted, {rejected} rejected (slowest rejection {slowest:?}), \
         {errors} transport errors, max in flight {max_in_flight}; peak heap growth {:.1} MB of {:.0} MB budget",
        plain.len() as f64 / MB,
        compressed.len() as f64 / MB,
        parse_peak as f64 / MB,
        single as f64 / MB,
        peak as f64 / MB,
        budget as f64 / MB,
    );
    assert!(accepted > 0, "the server made progress under overload");
    assert!(rejected > 0, "excess clients were turned away");
    assert_eq!(errors, 0, "every rejected client received the 429 rather than a reset connection");
    assert!(max_in_flight <= 2);
    assert!(slowest < Duration::from_secs(1), "rejections are prompt: slowest took {slowest:?}");
    assert!(
        peak < budget,
        "peak heap growth {:.1} MB exceeds the {:.0} MB budget",
        peak as f64 / MB,
        budget as f64 / MB
    );

    wait_until("slots freed", || ingest_in_flight(&s) == 0).await;
    s.stop().await;
}

// A flood of slow uploads, far more than the discard limits, all rejected
// at admission: discard tasks stop at MAX_DISCARD_PENDING (the rest are
// dropped at once), heap growth stays within the memory budget, and
// everything is let go after the discard timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flood_of_rejected_slow_uploads_is_bounded() {
    use server::routes::ingest::{MAX_DISCARD_PENDING, MAX_DISCARDING};
    const FLOOD: usize = 600;
    let _serial = SERIAL.lock().await;
    let s = TestServer::start_with(|c| {
        c.storage.max_memory = ByteSize(64 << 20);
        c.ingest.max_request_size = ByteSize(2 << 20);
        c.ingest.max_concurrent = 1;
        c.alerts.enabled = false;
    })
    .await;
    let budget = s.config.storage.max_memory.0 as usize;
    let headers = [("Content-Type", "application/x-ndjson")];
    let chunk = vec![b' '; 64 << 10];

    // Hold the only slot with an upload that never finishes.
    let mut holder = RawRequest::open(&s, "/api/v1/events", &headers, 1 << 20).await;
    holder.send(b"{}").await;
    wait_until("slot held", || ingest_in_flight(&s) == 1).await;
    let discarding = || MAX_DISCARD_PENDING - s.state().discard_pending.available_permits();
    let reading = || MAX_DISCARDING - s.state().discard_permits.available_permits();
    let rejected = || s.state().ingest_rejected.load(Ordering::Relaxed) as usize;

    let tasks_before = tokio::runtime::Handle::current().metrics().num_alive_tasks();
    let base = reset_peak();
    // Each client announces 1MB, sends 64KB and stalls.
    let mut flood = Vec::with_capacity(FLOOD);
    for _ in 0..FLOOD {
        let mut r = RawRequest::open(&s, "/api/v1/events", &headers, 1 << 20).await;
        r.send(&chunk).await;
        flood.push(r);
    }
    wait_until("flood rejected", || rejected() == FLOOD).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let tasks = tokio::runtime::Handle::current().metrics().num_alive_tasks() - tasks_before;
    let (held, active) = (discarding(), reading());

    // Every client that is answered gets the 429; those beyond the cap may
    // see their connection closed instead.
    let mut answered = 0;
    for r in &mut flood {
        if let Some(status) = r.status_line(Duration::from_millis(50)).await {
            assert_eq!(status, 429);
            answered += 1;
        }
    }
    let peak = PEAK.load(Ordering::Relaxed) - base;
    eprintln!(
        "REL-02 flood: {FLOOD} slow uploads rejected; {held} discard tasks ({active} reading, caps {MAX_DISCARD_PENDING}/{MAX_DISCARDING}); {tasks} new runtime tasks; {answered} read a 429; peak heap growth {:.1} MB of {:.0} MB budget",
        peak as f64 / MB,
        budget as f64 / MB,
    );
    assert_eq!(held, MAX_DISCARD_PENDING, "discard tasks stop at the cap");
    assert!(active <= MAX_DISCARDING);
    // One task per connection (hyper) plus at most the discard cap.
    assert!(tasks <= FLOOD + MAX_DISCARD_PENDING + 16, "{tasks} tasks for {FLOOD} connections");
    assert!(answered >= MAX_DISCARD_PENDING, "kept clients get the 429: {answered}");
    assert!(
        peak < budget,
        "peak heap growth {:.1} MB exceeds the {:.0} MB budget",
        peak as f64 / MB,
        budget as f64 / MB
    );

    // Stalled bodies are given up after the discard timeout.
    tokio::time::timeout(Duration::from_secs(15), async {
        while discarding() > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("discard tasks end after the discard timeout");
    drop(flood);
    drop(holder);
    wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
    assert_eq!(
        post(
            &s,
            &bytes_of(
                b"{\"message\":\"after\"}
"
                .to_vec()
            ),
            false
        )
        .await,
        200
    );
    s.stop().await;
}

fn bytes_of(v: impl Into<Vec<u8>>) -> axum::body::Bytes {
    axum::body::Bytes::from(v.into())
}

async fn post(s: &TestServer, body: &axum::body::Bytes, gz: bool) -> u16 {
    let mut req = s.client.post(s.u("/api/v1/events")).header("content-type", "application/x-ndjson");
    if gz {
        req = req.header("content-encoding", "gzip");
    }
    req.body(body.clone()).send().await.unwrap().status().as_u16()
}
