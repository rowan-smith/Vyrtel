//! Derived ingest admission under sustained overload (REL-14 TC-04,
//! REL-16 TC-03).
//!
//! Each configuration runs [`RUNS`] times in a row. A run starts a server
//! with `ingest.max_concurrent = 0` and sends maximum-size bodies of every
//! fixture (native JSON, NDJSON, CLEF, OTLP JSON and protobuf logs, traces
//! and metrics; plain and gzip), the high-amplification shapes among them,
//! from more clients than there are slots, retrying 429s after 5 ms, in two
//! phases. Bodies over the decoding limits get 413 (by event count or
//! decoded size, whichever the configuration reaches first); every other
//! status but 200 and 429 fails the run:
//!
//! 1. **Acknowledgement stalled.** Every signal's writer is parked, so
//!    admitted requests hold their slots with their parsed batches queued
//!    and nothing is written; everyone else is rejected and their bodies
//!    drained. Storage allocates nothing in this phase, so all heap growth
//!    is attributed to ingest: admitted requests, rejected-body drains, and
//!    the server's connection and runtime buffers (which over-counts
//!    slightly). The clients run on [`client_runtime`], whose allocations
//!    are not counted. It must stay within the ingest budget.
//! 2. **Writers running.** The writers are released and clients carry on.
//!    Heap growth now includes the WAL and the active segments (storage's
//!    budget); it is reported, with RSS, and must stay within
//!    `storage.max_memory`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use server::config::{ByteSize, Config, INGEST_FIXED_OVERHEAD};
use telemetry::Signal;

use super::fixtures::{self, Kind};
use super::*;

const RUNS: usize = 3;
const WARM_UP: Duration = Duration::from_secs(1);
const STALLED: Duration = Duration::from_secs(2);
const RUNNING: Duration = Duration::from_secs(2);

/// Current resident set size of this process, where the platform makes it
/// cheap to read.
pub fn rss() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        server::routes::system::process_rss()
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("(Get-Process -Id {}).WorkingSet64", std::process::id())])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok().map(|kb| kb * 1024)
    }
}

pub fn mb(n: Option<u64>) -> String {
    n.map_or("n/a".into(), |n| format!("{:.1} MB", n as f64 / MB))
}

struct Body {
    kind: Kind,
    gzip: bool,
    wire: Bytes,
    /// Over the decoding limits of the configuration it is sent to: 413.
    refused: bool,
}

/// Every fixture at the request-size limit of `config`, plain and gzip.
fn bodies(config: &Config) -> Vec<Body> {
    let limit = config.ingest.max_request_size.0 as usize;
    let mut out = Vec::new();
    for f in fixtures::all(limit) {
        assert!(f.body.len() <= limit && f.body.len() > limit * 9 / 10, "{} is near the limit", f.name);
        let refused = f.kind.parse(&f.body, config.ingest_limits()).is_err();
        out.push(Body { kind: f.kind, gzip: true, wire: Bytes::from(common::gzip(&f.body)), refused });
        out.push(Body { kind: f.kind, gzip: false, wire: Bytes::from(f.body), refused });
    }
    out
}

#[derive(Default)]
struct Counts {
    accepted: AtomicU64,
    rejected: AtomicU64,
    /// 413 for a body over the decoding limits.
    refused: AtomicU64,
    other: AtomicU64,
    errors: AtomicU64,
}

/// One run. `enforce` holds the run to the ingest budget; without it the
/// figures are only reported (the pre-REL-14 comparison).
async fn run(name: &str, configure: fn(&mut Config), bodies: &Arc<Vec<Body>>, run: usize, enforce: bool) {
    let s = TestServer::start_with(|c| {
        configure(c);
        c.alerts.enabled = false;
    })
    .await;
    let admission = s.config.ingest_admission();
    let (limit, budget) = (admission.limit, admission.budget as usize);
    let max_memory = s.config.storage.max_memory.0 as usize;
    assert!(limit >= 1);
    let clients = 4 * limit + 16;

    let stalls: Vec<_> = Signal::ALL.into_iter().map(|sig| stall_writer(&s, sig)).collect();
    // Warm-up: every slot held by the test, so all clients are rejected and
    // their connections' read buffers grow to their working size, which
    // they keep for the life of the connection. That is per-connection HTTP
    // overhead, not ingest's; it is reported and left out of the window.
    let held = s.state().ingest_permits.clone().try_acquire_many_owned(limit as u32).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Load clients get their own connection pool: their connections live on clients_rt, and
    // once it shuts down a pooled one is dead (DispatchGone) if s.client later reuses it.
    let load = authorized_client("authorization", &format!("Bearer {}", s.api_key));
    let rss_before = rss();
    let start = reset_peak();

    let clients_rt = client_runtime();
    let counts = Arc::new(Counts::default());
    let stop = Instant::now() + WARM_UP + STALLED + RUNNING;
    let mut tasks = Vec::new();
    for i in 0..clients {
        let (client, url, bodies, counts) = (load.clone(), s.url.clone(), bodies.clone(), counts.clone());
        tasks.push(clients_rt.spawn(async move {
            let mut n = i;
            while Instant::now() < stop {
                let b = &bodies[n % bodies.len()];
                n += 1;
                let mut req =
                    client.post(format!("{url}{}", b.kind.path())).header("content-type", b.kind.content_type());
                if b.gzip {
                    req = req.header("content-encoding", "gzip");
                }
                match req.body(b.wire.clone()).send().await {
                    Ok(r) if r.status() == 200 && !b.refused => counts.accepted.fetch_add(1, Ordering::Relaxed),
                    Ok(r) if r.status() == 413 && b.refused => counts.refused.fetch_add(1, Ordering::Relaxed),
                    Ok(r) if r.status() == 429 => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        counts.rejected.fetch_add(1, Ordering::Relaxed)
                    }
                    Ok(r) => {
                        eprintln!("unexpected {} from {} (refused: {})", r.status(), b.kind.path(), b.refused);
                        counts.other.fetch_add(1, Ordering::Relaxed)
                    }
                    Err(e) => {
                        eprintln!("transport error: {e}");
                        counts.errors.fetch_add(1, Ordering::Relaxed)
                    }
                };
            }
        }));
    }

    tokio::time::sleep(WARM_UP).await;
    let base = reset_peak();
    let connections = base.saturating_sub(start);
    let rejected_warm = counts.rejected.load(Ordering::Relaxed);
    drop(held);

    // Phase 1: saturated, nothing acknowledged.
    let phase1_end = Instant::now() + STALLED;
    let mut max_in_flight = 0;
    while Instant::now() < phase1_end {
        max_in_flight = max_in_flight.max(ingest_in_flight(&s));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let attributed = PEAK.load(Ordering::Relaxed) - base;
    let rss_stalled = rss();
    let rejected_stalled = counts.rejected.load(Ordering::Relaxed) - rejected_warm;
    assert_eq!(counts.accepted.load(Ordering::Relaxed), 0, "nothing is acknowledged while the writers are parked");

    // Phase 2: writers running.
    drop(stalls);
    while Instant::now() < stop {
        max_in_flight = max_in_flight.max(ingest_in_flight(&s));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    for t in tasks {
        t.await.unwrap();
    }
    let process = PEAK.load(Ordering::Relaxed) - base;
    let rss_after = rss();
    clients_rt.shutdown_background();
    let (accepted, rejected) =
        (counts.accepted.load(Ordering::Relaxed), counts.rejected.load(Ordering::Relaxed) - rejected_warm);
    let (other, errors) = (counts.other.load(Ordering::Relaxed), counts.errors.load(Ordering::Relaxed));
    let refused = counts.refused.load(Ordering::Relaxed);
    let info = s.get_ok("/api/v1/system/info").await;

    eprintln!(
        "| {name} | {run} | {limit} | {clients} | {:.1} | {:.1} | {:.1} | {max_in_flight} | {rejected_stalled} | {accepted} | {rejected} | {refused} | {:.1} | {} | {} | {} |",
        budget as f64 / MB,
        connections as f64 / MB,
        attributed as f64 / MB,
        process as f64 / MB,
        mb(rss_before),
        mb(rss_stalled),
        mb(rss_after),
    );
    assert_eq!(info["ingest"]["maxConcurrent"], limit, "system info reports the effective limit");
    assert_eq!(info["ingest"]["concurrencySource"], if admission.derived { "derived" } else { "explicit" });
    assert_eq!((other, errors), (0, 0), "only 200, 429 and 413 for bodies over the limits, and no reset connections");
    wait_until("slots freed", || ingest_in_flight(&s) == 0).await;
    assert_eq!(s.post("/api/v1/events", serde_json::json!({ "message": "after" })).await.status(), 200);
    s.stop().await;
    if !enforce {
        return;
    }
    assert!(admission.derived);
    assert_eq!(max_in_flight, limit, "admission saturated at the derived limit and never above it");
    assert!(rejected_stalled > 0 && accepted > 0, "excess clients were turned away and the server made progress");
    assert!(refused > 0, "bodies over the decoding limits were refused");
    assert!(
        attributed <= budget,
        "{name} run {run}: ingest heap growth {:.1} MB exceeds the {:.1} MB ingest budget",
        attributed as f64 / MB,
        budget as f64 / MB
    );
    assert!(process <= max_memory, "{name} run {run}: process heap growth {:.1} MB", process as f64 / MB);
}

fn default_config(_: &mut Config) {}

/// A small `max_memory` whose budget fits exactly one request.
fn constrained_config(c: &mut Config) {
    c.storage.max_memory = ByteSize(128 << 20);
    c.ingest.max_request_size = ByteSize(256 << 10);
}

fn print_header() {
    eprintln!(
        "\nREL-14 overload, {} build, {}/{}; {STALLED:?} with writers parked, then {RUNNING:?} running",
        if cfg!(debug_assertions) { "debug" } else { "release" },
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    eprintln!(
        "| Config | Run | Limit | Clients | Ingest budget MB | Connection buffers MB | Ingest heap peak MB (writers parked) | Max in flight | 429s (parked) | 200s | 429s | 413s | Process heap peak MB | RSS before | RSS parked | RSS after |"
    );
    eprintln!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn derived_admission_holds_the_ingest_budget_under_overload() {
    let _serial = SERIAL.lock().await;
    print_header();
    for (name, configure) in [
        ("default (512MB, 1MB)", default_config as fn(&mut Config)),
        ("constrained (128MB, 256KB)", constrained_config),
    ] {
        let mut c = Config::default();
        configure(&mut c);
        let bodies = Arc::new(bodies(&c));
        for i in 1..=RUNS {
            run(name, configure, &bodies, i, true).await;
        }
    }
}

/// The pre-REL-14 defaults: 16MB requests and the 4 slots the old formula
/// derived at 512MB. Reported, not asserted; run with `--ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "comparison figures for docs/testing.md; needs ~2 GB of memory"]
async fn pre_rel14_defaults_under_the_same_overload() {
    let _serial = SERIAL.lock().await;
    print_header();
    let configure: fn(&mut Config) = |c| {
        c.ingest.max_request_size = ByteSize(16 << 20);
        c.ingest.max_concurrent = 4;
        // 16MB batches hold up to ~90,000 events; keep them under the
        // queue's batch limit so they are parsed and queued, not refused.
        c.ingest.queue_capacity = 200_000;
    };
    let mut c = Config::default();
    configure(&mut c);
    let bodies = Arc::new(bodies(&c));
    for i in 1..=RUNS {
        run("pre-REL-14 (512MB, 16MB, 4 slots)", configure, &bodies, i, false).await;
    }
}

// Rejected bodies drained at full speed, all slots held: the drain's heap
// cost (plus connection buffers) stays within the fixed ingest overhead
// that derived sizing reserves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_body_drain_overhead_is_within_the_fixed_allowance() {
    use server::routes::ingest::MAX_DISCARDING;
    let _serial = SERIAL.lock().await;
    let clients: usize = std::env::var("DRAIN_CLIENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(64);
    let s = TestServer::start_with(|c| {
        c.ingest.max_concurrent = 1;
        c.ingest.max_request_size = ByteSize(1 << 20);
        c.alerts.enabled = false;
    })
    .await;
    let mut holder = RawRequest::open(&s, "/api/v1/events", &[("Content-Type", "application/x-ndjson")], 1 << 20).await;
    holder.send(b"{}").await;
    wait_until("slot held", || ingest_in_flight(&s) == 1).await;

    let plain = Bytes::from(ndjson_payload(5_000, 160).into_bytes()[..1 << 20].to_vec());
    let gz = Bytes::from(common::gzip(&plain));
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Load clients get their own connection pool: their connections live on clients_rt, and
    // once it shuts down a pooled one is dead (DispatchGone) if s.client later reuses it.
    let load = authorized_client("authorization", &format!("Bearer {}", s.api_key));
    let start = reset_peak();
    let stop = Instant::now() + Duration::from_secs(3);
    let rejected = Arc::new(AtomicU64::new(0));
    let clients_rt = client_runtime();
    let mut tasks = Vec::new();
    for i in 0..clients {
        let (client, url, rejected) = (load.clone(), s.u("/api/v1/events"), rejected.clone());
        let body = if i % 2 == 0 { plain.clone() } else { gz.clone() };
        tasks.push(clients_rt.spawn(async move {
            while Instant::now() < stop {
                let mut req = client.post(&url).header("content-type", "application/x-ndjson");
                if i % 2 == 1 {
                    req = req.header("content-encoding", "gzip");
                }
                let r = req.body(body.clone()).send().await.unwrap();
                assert_eq!(r.status(), 429);
                rejected.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }
    // Warm up: every connection's read buffer grows to its working size
    // and stays there for the life of the connection. That is per-
    // connection HTTP overhead, reported separately; what is measured
    // after it is the drain itself.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let base = reset_peak();
    let connections = base.saturating_sub(start);
    for t in tasks {
        t.await.unwrap();
    }
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    clients_rt.shutdown_background();
    eprintln!(
        "REL-14 drain: {clients} clients, {} rejected 1 MB bodies drained ({MAX_DISCARDING} at once); connection buffers \
         {:.2} MB ({:.0} KB each, not ingest-owned); drain peak heap growth {:.2} MB of {:.2} MB allowance",
        rejected.load(Ordering::Relaxed),
        connections as f64 / MB,
        connections as f64 / clients as f64 / 1024.0,
        peak as f64 / MB,
        INGEST_FIXED_OVERHEAD as f64 / MB,
    );
    assert!(rejected.load(Ordering::Relaxed) as usize > clients);
    assert!(
        peak <= INGEST_FIXED_OVERHEAD as usize,
        "drain overhead {:.2} MB exceeds the {:.2} MB fixed allowance",
        peak as f64 / MB,
        INGEST_FIXED_OVERHEAD as f64 / MB
    );
    drop(holder);
    wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
    s.stop().await;
}
