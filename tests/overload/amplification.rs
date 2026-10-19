//! Ingest memory amplification per format (REL-14 TC-03, REL-16 TC-02).
//!
//! For every fixture in [`fixtures::all`], sent plain and gzip-compressed:
//!
//! * **parse**: heap peak of decoding alone, over the already-read body
//!   (`ingest::native::parse` / `ingest::otlp::decode_*` with the server's
//!   decoding limits, run in-process);
//! * **admission**: heap peak from the moment the request is sent until
//!   its batch sits in the storage queue, the writer parked so nothing is
//!   written yet, or until its 413 when decoding is refused. This is what
//!   an admitted request owns: the read (and gzip-decoded) body, the parse
//!   working set and the parsed batch;
//! * **end to end**: the same window extended until the response, after
//!   the writer has encoded the WAL record and appended the batch to the
//!   active segment. The WAL record is streamed to the file as it is
//!   encoded, so writing adds next to nothing: this is held to the
//!   estimate too.
//!
//! Every figure is divided by the decoded body size, so it reads directly
//! as a multiple of `ingest.max_request_size`. The test asserts, for every
//! fixture with no exemptions, that the admission and end-to-end peaks stay
//! within
//! [`server::config::INGEST_BYTES_PER_REQUEST_BYTE`] (the factor the
//! derived limit is computed with) and that the server accepts or refuses
//! it as the fixture expects ([`fixtures::Expect`]).
//!
//! ```text
//! cargo test -p server --release --test overload amplification -- --nocapture
//! ```
//!
//! `VYRTEL_AMPLIFICATION_TARGET` (bytes, default 2 MB) changes the decoded
//! fixture size.

use std::sync::atomic::Ordering;

use server::config::{ByteSize, Config, INGEST_BYTES_PER_REQUEST_BYTE};
use telemetry::Signal;

use super::fixtures::{self, Expect, Fixture, Kind};
use super::*;

struct Row {
    name: String,
    gzip: bool,
    wire: usize,
    decoded: usize,
    events: usize,
    parse: usize,
    /// Whether decoding alone, in-process, accepted it.
    parsed: bool,
    admission: usize,
    end_to_end: usize,
    status: u16,
    expect: Expect,
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

/// The configuration every fixture is measured with: requests exactly as
/// large as the fixture, and a queue that never limits the batch, so the
/// decoded-bytes limit is the one under test.
fn configure(c: &mut Config, f: &Fixture) {
    c.storage.max_memory = ByteSize(4 << 30);
    c.ingest.max_request_size = ByteSize(f.body.len() as u64);
    c.ingest.max_concurrent = 1;
    c.ingest.queue_capacity = 10_000_000;
    c.alerts.enabled = false;
}

/// Decoding alone, over a body that is already in memory; whether it was
/// accepted.
fn parse_peak(f: &Fixture) -> (usize, bool) {
    let mut c = Config::default();
    configure(&mut c, f);
    let base = reset_peak();
    let events = f.kind.parse(&f.body, c.ingest_limits());
    let peak = PEAK.load(Ordering::Relaxed) - base;
    let accepted = match &events {
        Ok(events) => {
            assert_eq!(events.len(), f.events, "{}", f.name);
            true
        }
        Err(ingest::IngestError::TooLarge { .. }) => false,
        Err(e) => panic!("{}: {e}", f.name),
    };
    drop(events);
    (peak, accepted)
}

/// Send `f` to a fresh server with the writer parked; measure until the
/// batch is queued (or refused), then release the writer and measure until
/// the response. A fresh server per request keeps earlier requests'
/// sealing out of the window.
async fn request_peaks_on(s: &TestServer, f: &Fixture, gzip: bool, wire: &axum::body::Bytes) -> (usize, usize, u16) {
    let signal = signal(f.kind);
    let release = stall_writer(s, signal);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let base = reset_peak();
    let mut req = s.client.post(s.u(f.kind.path())).header("content-type", f.kind.content_type());
    if gzip {
        req = req.header("content-encoding", "gzip");
    }
    let pending = tokio::spawn(req.body(wire.clone()).send());
    wait_until("batch queued or refused", || queue_depth(s, signal) == f.events || pending.is_finished()).await;
    let admission = PEAK.load(Ordering::Relaxed) - base;
    drop(release);
    let r = pending.await.unwrap().unwrap();
    let status = r.status().as_u16();
    if status == 413 {
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["error"]["limit"], "maxDecodedBytes", "{}: {body}", f.name);
        assert_eq!(queue_depth(s, signal), 0, "{}: nothing of a refused batch is queued", f.name);
    } else {
        drop(r);
    }
    let end_to_end = PEAK.load(Ordering::Relaxed) - base;
    (admission, end_to_end, status)
}

async fn measure(f: &Fixture, gzip: bool, wire: &axum::body::Bytes) -> (usize, usize, u16) {
    let s = TestServer::start_with(|c| configure(c, f)).await;
    let peaks = request_peaks_on(&s, f, gzip, wire).await;
    wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
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
    let parse: Vec<(usize, bool)> = fixtures.iter().map(parse_peak).collect();

    let mut rows = Vec::new();
    for (f, &(parse, decoded)) in fixtures.iter().zip(&parse) {
        for gzip in [false, true] {
            let wire = axum::body::Bytes::from(if gzip { common::gzip(&f.body) } else { f.body.clone() });
            let (admission, end_to_end, status) = measure(f, gzip, &wire).await;
            rows.push(Row {
                name: f.name.clone(),
                gzip,
                wire: wire.len(),
                decoded: f.body.len(),
                events: f.events,
                parse,
                parsed: decoded,
                admission,
                end_to_end,
                status,
                expect: f.expect,
                shape: f.shape,
            });
        }
    }

    eprintln!(
        "\nREL-16 amplification, seed {:#x}, target {:.2} MB decoded, {} build, {}/{}; factor {INGEST_BYTES_PER_REQUEST_BYTE}",
        fixtures::SEED,
        target as f64 / MB,
        if cfg!(debug_assertions) { "debug" } else { "release" },
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    eprintln!(
        "| Fixture | Encoding | Wire MB | Decoded MB | Events | Parse ×body | Admission ×body | End to end ×body | Status | Record shape |"
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
            r.status,
            r.shape,
        );
    }
    let worst =
        |expect| rows.iter().filter(|r| r.expect == expect).map(|r| x(r.admission, r.decoded)).fold(0.0, f64::max);
    eprintln!(
        "worst admission peak {:.2}× body accepted, {:.2}× refused; estimate {INGEST_BYTES_PER_REQUEST_BYTE}×\n",
        worst(Expect::Accepted),
        worst(Expect::Refused)
    );

    for r in &rows {
        let what = format!("{} ({})", r.name, if r.gzip { "gzip" } else { "plain" });
        let expected = match r.expect {
            Expect::Accepted => 200,
            Expect::Refused => 413,
        };
        assert_eq!(r.status, expected, "{what}");
        assert_eq!(r.parsed, r.expect == Expect::Accepted, "{what}: decoded in-process");
        assert!(
            r.admission <= r.decoded * INGEST_BYTES_PER_REQUEST_BYTE as usize,
            "{what}: admission peak {:.2}× body exceeds the {INGEST_BYTES_PER_REQUEST_BYTE}× estimate",
            x(r.admission, r.decoded),
        );
        assert!(
            r.end_to_end <= r.decoded * INGEST_BYTES_PER_REQUEST_BYTE as usize,
            "{what}: end-to-end peak {:.2}× body exceeds the {INGEST_BYTES_PER_REQUEST_BYTE}× estimate",
            x(r.end_to_end, r.decoded),
        );
    }
}
