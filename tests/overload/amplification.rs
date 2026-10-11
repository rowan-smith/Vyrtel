//! Ingest memory amplification per format (REL-14, TC-03).
//!
//! For every fixture in [`fixtures::all`], sent plain and gzip-compressed:
//!
//! * **parse**: heap peak of decoding alone, over the already-read body
//!   (`ingest::native::parse` / `ingest::otlp::decode_*`, run in-process);
//! * **admission**: heap peak from the moment the request is sent until
//!   its batch sits in the storage queue, the writer parked so nothing is
//!   written yet. This is what an admitted request owns: the read (and
//!   gzip-decoded) body, the parse working set and the parsed batch;
//! * **end to end**: the same window extended until the response, after
//!   the writer has encoded the WAL record and appended the batch to the
//!   active segment. The difference is storage's, budgeted under active
//!   segments, not ingest.
//!
//! Every figure is divided by the decoded body size, so it reads directly
//! as a multiple of `ingest.max_request_size`. The test asserts that each
//! supported fixture's admission peak stays within
//! [`server::config::INGEST_BYTES_PER_REQUEST_BYTE`], the factor the
//! derived limit is computed with.
//!
//! ```text
//! cargo test -p server --release --test overload amplification -- --nocapture
//! ```
//!
//! `VYRTEL_AMPLIFICATION_TARGET` (bytes, default 2 MB) changes the decoded
//! fixture size.

use std::sync::atomic::Ordering;

use server::config::{ByteSize, INGEST_BYTES_PER_REQUEST_BYTE};
use telemetry::Signal;

use super::fixtures::{self, Fixture, Kind};
use super::*;

struct Row {
    name: String,
    gzip: bool,
    wire: usize,
    decoded: usize,
    events: usize,
    parse: usize,
    admission: usize,
    end_to_end: usize,
    supported: bool,
    shape: &'static str,
}

fn signal(kind: Kind) -> Signal {
    match kind {
        Kind::NativeJson | Kind::NativeNdJson | Kind::OtlpLogs(_) => Signal::Logs,
        Kind::OtlpTraces(_) => Signal::Traces,
        Kind::OtlpMetrics(_) => Signal::Metrics,
    }
}

fn x(bytes: usize, of: usize) -> f64 {
    bytes as f64 / of as f64
}

/// Decoding alone, over a body that is already in memory.
fn parse_peak(f: &Fixture) -> usize {
    let base = reset_peak();
    let events = f.kind.parse(&f.body);
    let peak = PEAK.load(Ordering::Relaxed) - base;
    assert_eq!(events.len(), f.events, "{}", f.name);
    drop(events);
    peak
}

/// Send `f` to a fresh server with the writer parked; measure until the
/// batch is queued, then release the writer and measure until the
/// response. A fresh server per request keeps earlier requests' sealing
/// out of the window.
async fn request_peaks_on(s: &TestServer, f: &Fixture, gzip: bool, wire: &axum::body::Bytes) -> (usize, usize) {
    let signal = signal(f.kind);
    let release = stall_writer(s, signal);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let base = reset_peak();
    let mut req = s.client.post(s.u(f.kind.path())).header("content-type", f.kind.content_type());
    if gzip {
        req = req.header("content-encoding", "gzip");
    }
    let pending = tokio::spawn(req.body(wire.clone()).send());
    wait_until("batch queued", || queue_depth(s, signal) == f.events).await;
    let admission = PEAK.load(Ordering::Relaxed) - base;
    drop(release);
    let r = pending.await.unwrap().unwrap();
    assert_eq!(r.status(), 200, "{}", f.name);
    drop(r);
    let end_to_end = PEAK.load(Ordering::Relaxed) - base;
    (admission, end_to_end)
}

async fn measure(f: &Fixture, gzip: bool, wire: &axum::body::Bytes) -> (usize, usize) {
    let s = TestServer::start_with(|c| {
        c.storage.max_memory = ByteSize(4 << 30);
        c.ingest.max_request_size = ByteSize(f.body.len() as u64);
        c.ingest.max_concurrent = 1;
        c.ingest.queue_capacity = 10_000_000;
        c.alerts.enabled = false;
    })
    .await;
    let peaks = request_peaks_on(&s, f, gzip, wire).await;
    s.stop().await;
    peaks
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn amplification_per_format_is_within_the_admission_estimate() {
    let _serial = SERIAL.lock().await;
    let target: usize =
        std::env::var("VYRTEL_AMPLIFICATION_TARGET").ok().and_then(|v| v.parse().ok()).unwrap_or(2 << 20);
    let fixtures = fixtures::all(target);
    // Decoding alone first, with no server running.
    let parse: Vec<usize> = fixtures.iter().map(parse_peak).collect();

    let mut rows = Vec::new();
    for (f, parse) in fixtures.iter().zip(parse) {
        for gzip in [false, true] {
            let wire = axum::body::Bytes::from(if gzip { common::gzip(&f.body) } else { f.body.clone() });
            let (admission, end_to_end) = measure(f, gzip, &wire).await;
            rows.push(Row {
                name: f.name.clone(),
                gzip,
                wire: wire.len(),
                decoded: f.body.len(),
                events: f.events,
                parse,
                admission,
                end_to_end,
                supported: f.supported,
                shape: f.shape,
            });
        }
    }

    eprintln!(
        "\nREL-14 amplification, seed {:#x}, target {:.2} MB decoded, {} build, {}/{}; factor {INGEST_BYTES_PER_REQUEST_BYTE}",
        fixtures::SEED,
        target as f64 / MB,
        if cfg!(debug_assertions) { "debug" } else { "release" },
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    eprintln!(
        "| Fixture | Encoding | Wire MB | Decoded MB | Events | Parse ×body | Admission ×body | End to end ×body | Supported | Record shape |"
    );
    eprintln!("|---|---|---:|---:|---:|---:|---:|---:|:---:|---|");
    for r in &rows {
        eprintln!(
            "| {} | {} | {:.2} | {:.2} | {} | {:.2} | {:.2} | {:.2} | {} | {} |",
            r.name,
            if r.gzip { "gzip" } else { "plain" },
            r.wire as f64 / MB,
            r.decoded as f64 / MB,
            r.events,
            x(r.parse, r.decoded),
            x(r.admission, r.decoded),
            x(r.end_to_end, r.decoded),
            if r.supported { "yes" } else { "no" },
            r.shape,
        );
    }
    let worst = rows.iter().filter(|r| r.supported).map(|r| x(r.admission, r.decoded)).fold(0.0, f64::max);
    eprintln!("worst supported admission peak {worst:.2}× body; estimate {INGEST_BYTES_PER_REQUEST_BYTE}×\n");

    for r in rows.iter().filter(|r| r.supported) {
        assert!(
            r.admission <= r.decoded * INGEST_BYTES_PER_REQUEST_BYTE as usize,
            "{} ({}): admission peak {:.2}× body exceeds the {INGEST_BYTES_PER_REQUEST_BYTE}× estimate",
            r.name,
            if r.gzip { "gzip" } else { "plain" },
            x(r.admission, r.decoded),
        );
    }
}
