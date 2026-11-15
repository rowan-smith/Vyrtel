//! Connection-pressure load for Vyrtel's transport limits (REL-18).
//!
//! ```text
//! cargo run --release -p loadgen --bin connload -- --target http://127.0.0.1:8080 \
//!     --duration 30m --attempts 1000 --wave-interval 60s --out results/soak
//! ```
//!
//! For `--duration`, keeps a steady mixed load (ingest batches on
//! keep-alive connections, plus count queries) and, every
//! `--wave-interval`, opens `--attempts` connections at once, a quarter
//! each of:
//!
//! * `idle`: connects and sends nothing;
//! * `slow-head`: sends a request head one byte every 500 ms;
//! * `slow-body`: sends a complete head announcing a 100 KB body, then 10
//!   bytes, then stalls;
//! * `normal`: `GET /health` with `Connection: close`.
//!
//! Each attempt is classified by what it got back: `served` (200),
//! `refused` (the fixed 503 `server_busy`, or part of it), `rejected` (429
//! from ingest admission), `timeout` (408),
//! `closed` (no bytes, then EOF), `reset` (a connection error) or `other`.
//!
//! Each wave's timing is split in two, because the server's own deadlines
//! start at accept while a client waits from its first attempt:
//!
//! * `establishment`: from the attempt's start until the server's kernel had
//!   completed the handshake. On Linux this is when the attempt's first
//!   bytes were acknowledged (`TIOCOUTQ` drops to 0): when the listen queue
//!   is full the server drops the handshake and the client keeps
//!   retransmitting, even though its own `connect` may already have
//!   returned. Idle attempts send nothing, so for them (and on other
//!   systems) it is when `connect` returned, a lower bound. `connect` is
//!   reported on its own too.
//! * `acceptToEnd`: from establishment until the connection ended: the part
//!   the server's `header_timeout`, `body_timeout` and waiting bound.
//!
//! `endToEnd` is the two together; its maximum is about the wave's `tookS`.
//! An attempt still open when `--wave-patience` runs out is `abandoned`:
//! the server never ended it (an idle connection whose handshake the server
//! dropped exists only on the client's side), so it has no `acceptToEnd`,
//! and no `establishment` unless its bytes were acknowledged. The target is
//! resolved once, so name lookups are not part of `connect`.
//! `/api/v1/system/info` is sampled every `--sample-interval` into
//! `samples.csv`. At the end the events the server acknowledged are
//! counted back with a query (`persisted` must equal `acknowledged`), and
//! `summary.json` holds every total, latency percentile and peak.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Parser, Clone)]
#[command(name = "connload", about = "Connection-pressure load against Vyrtel")]
struct Args {
    /// Vyrtel base URL (http only).
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    target: String,
    /// How long to run, e.g. 90s, 30m.
    #[arg(long, default_value = "60s", value_parser = parse_duration)]
    duration: Duration,
    /// Connection attempts per wave, all at once.
    #[arg(long, default_value_t = 1000)]
    attempts: usize,
    /// Time between the starts of waves (the first starts after 5 s).
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    wave_interval: Duration,
    /// How long one wave's connections are given to end on their own, once
    /// connected.
    #[arg(long, default_value = "90s", value_parser = parse_duration)]
    wave_patience: Duration,
    /// How long one attempt may take to connect before it counts as `reset`.
    /// Longer than Linux's SYN retries (1 + 2 + 4 + 8 + 16 s), so a dropped
    /// handshake shows in `establishment` instead of being cut short.
    #[arg(long, default_value = "90s", value_parser = parse_duration)]
    connect_timeout: Duration,
    /// Concurrent ingest senders (keep-alive connections).
    #[arg(long, default_value_t = 4)]
    ingest_clients: usize,
    /// Events per ingest request.
    #[arg(long, default_value_t = 200)]
    batch: usize,
    /// Pause between one sender's requests.
    #[arg(long, default_value = "50ms", value_parser = parse_duration)]
    ingest_interval: Duration,
    /// Concurrent query senders.
    #[arg(long, default_value_t = 2)]
    query_clients: usize,
    /// Pause between one query sender's requests.
    #[arg(long, default_value = "1s", value_parser = parse_duration)]
    query_interval: Duration,
    /// How often to sample /api/v1/system/info.
    #[arg(long, default_value = "1s", value_parser = parse_duration)]
    sample_interval: Duration,
    /// Run only the steady load, without connection waves (for baselines).
    #[arg(long)]
    no_waves: bool,
    /// Directory for summary.json and samples.csv.
    #[arg(long, default_value = "connload-out")]
    out: PathBuf,
    /// Label recorded in the summary and used to tag events.
    #[arg(long, default_value = "run")]
    label: String,
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else {
        (s, 1.0)
    };
    let v: f64 = num.parse().map_err(|_| format!("invalid duration '{s}'"))?;
    Ok(Duration::from_secs_f64(v * mult))
}

const KINDS: [&str; 4] = ["idle", "slow-head", "slow-body", "normal"];
const OUTCOMES: [&str; 7] = ["served", "refused", "rejected", "timeout", "closed", "reset", "other"];

/// The start of the fixed refusal response (`transport::BUSY_RESPONSE`).
const BUSY_PREFIX: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n";

/// Count, p50, p95, p99 and maximum of `micros`, in milliseconds.
fn distribution(mut micros: Vec<u32>) -> Value {
    micros.sort_unstable();
    let p = |q: f64| -> Value {
        if micros.is_empty() {
            return Value::Null;
        }
        let i = ((micros.len() as f64 - 1.0) * q).round() as usize;
        json!(micros[i] as f64 / 1000.0)
    };
    json!({ "samples": micros.len(), "p50Ms": p(0.50), "p95Ms": p(0.95), "p99Ms": p(0.99), "maxMs": p(1.0) })
}

fn micros(d: Duration) -> u32 {
    d.as_micros().min(u32::MAX as u128) as u32
}

#[derive(Default)]
struct Latencies(Mutex<Vec<u32>>);

impl Latencies {
    fn record(&self, d: Duration) {
        self.0.lock().unwrap().push(micros(d));
    }

    fn summary(&self) -> Value {
        distribution(self.0.lock().unwrap().clone())
    }
}

#[derive(Default)]
struct Steady {
    ingest_ok: AtomicU64,
    acknowledged: AtomicU64,
    ingest_429: AtomicU64,
    ingest_503: AtomicU64,
    ingest_other: AtomicU64,
    ingest_errors: AtomicU64,
    query_ok: AtomicU64,
    query_503: AtomicU64,
    query_other: AtomicU64,
    query_errors: AtomicU64,
    ingest_latency: Latencies,
    query_latency: Latencies,
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()
}

fn host_port(target: &str) -> anyhow::Result<String> {
    let rest = target.strip_prefix("http://").context("--target must start with http://")?;
    Ok(rest.trim_end_matches('/').to_string())
}

async fn ingest_sender(args: Args, client: reqwest::Client, steady: Arc<Steady>, until: Instant, id: usize) {
    let url = format!("{}/api/v1/events", args.target);
    let service = format!("connload-{}", args.label);
    let mut seq = 0u64;
    while Instant::now() < until {
        let body: String = (0..args.batch)
            .map(|i| {
                seq += 1;
                format!(
                    "{{\"message\":\"connload {id}-{seq}-{i}\",\"service\":\"{service}\",\"level\":\"Information\",\"properties\":{{\"sender\":{id},\"seq\":{seq}}}}}\n"
                )
            })
            .collect();
        let started = Instant::now();
        match client.post(&url).header("content-type", "application/x-ndjson").body(body).send().await {
            Ok(r) => {
                let status = r.status().as_u16();
                let v: Value = r.json().await.unwrap_or(Value::Null);
                match status {
                    200 => {
                        steady.ingest_latency.record(started.elapsed());
                        steady.ingest_ok.fetch_add(1, Ordering::Relaxed);
                        steady.acknowledged.fetch_add(v["accepted"].as_u64().unwrap_or(0), Ordering::Relaxed);
                    }
                    429 => drop(steady.ingest_429.fetch_add(1, Ordering::Relaxed)),
                    503 => drop(steady.ingest_503.fetch_add(1, Ordering::Relaxed)),
                    _ => drop(steady.ingest_other.fetch_add(1, Ordering::Relaxed)),
                }
            }
            Err(_) => drop(steady.ingest_errors.fetch_add(1, Ordering::Relaxed)),
        }
        tokio::time::sleep(args.ingest_interval).await;
    }
}

async fn query_sender(args: Args, client: reqwest::Client, steady: Arc<Steady>, until: Instant) {
    let url = format!("{}/api/v1/query/logs/count", args.target);
    let body = json!({ "query": format!("service = connload-{} and level = Information", args.label) });
    while Instant::now() < until {
        let started = Instant::now();
        match client.post(&url).json(&body).send().await {
            Ok(r) => match r.status().as_u16() {
                200 => {
                    let _ = r.bytes().await;
                    steady.query_latency.record(started.elapsed());
                    steady.query_ok.fetch_add(1, Ordering::Relaxed);
                }
                503 => drop(steady.query_503.fetch_add(1, Ordering::Relaxed)),
                _ => drop(steady.query_other.fetch_add(1, Ordering::Relaxed)),
            },
            Err(_) => drop(steady.query_errors.fetch_add(1, Ordering::Relaxed)),
        }
        tokio::time::sleep(args.query_interval).await;
    }
}

const SLOW_HEAD: &[u8] = b"GET /health HTTP/1.1\r\nHost: connload\r\nX-Slow: ";
const SLOW_BODY_HEAD: &[u8] = b"POST /api/v1/events HTTP/1.1\r\nHost: connload\r\nContent-Type: application/json\r\nContent-Length: 100000\r\n\r\n[{\"message\"";
const NORMAL_REQUEST: &[u8] = b"GET /health HTTP/1.1\r\nHost: connload\r\nConnection: close\r\n\r\n";

/// When things happened during one attempt, from its start.
#[derive(Debug, Clone, Copy, Default)]
struct Timing {
    /// `connect` returned: the client's side of the handshake.
    connected: Option<Duration>,
    /// The server's kernel acknowledged the attempt's first bytes, so its
    /// side of the handshake was complete. `None` for idle attempts and
    /// where it cannot be observed.
    acked: Option<Duration>,
    /// The connection ended, or the attempt gave up.
    ended: Duration,
    /// Still open when `--wave-patience` ran out: the server never ended
    /// it, typically because it never had it (an idle connection whose
    /// handshake the server dropped is open only on the client's side).
    abandoned: bool,
}

impl Timing {
    /// Connection establishment as the server saw it, or its lower bound.
    /// `None` if it never connected, or if it sent nothing and was
    /// abandoned: then whether the server ever had it is unknown.
    fn established(&self) -> Option<Duration> {
        if self.abandoned && self.acked.is_none() {
            return None;
        }
        self.acked.or(self.connected)
    }

    /// From establishment until the server ended the connection; `None` if
    /// it did not end it.
    fn accept_to_end(&self) -> Option<Duration> {
        if self.abandoned {
            return None;
        }
        self.established().map(|e| self.ended.saturating_sub(e))
    }
}

/// One connection attempt of `kind`; returns its outcome and timing.
async fn attempt(
    addr: SocketAddr,
    kind: usize,
    connect_timeout: Duration,
    patience: Duration,
) -> (&'static str, Timing) {
    let started = Instant::now();
    let mut timing = Timing::default();
    let outcome = run_attempt(addr, kind, connect_timeout, patience, started, &mut timing).await;
    timing.ended = started.elapsed();
    if let (Some(a), Some(c)) = (timing.acked, timing.connected) {
        timing.acked = Some(a.max(c).min(timing.ended));
    }
    (outcome, timing)
}

async fn run_attempt(
    addr: SocketAddr,
    kind: usize,
    connect_timeout: Duration,
    patience: Duration,
    started: Instant,
    timing: &mut Timing,
) -> &'static str {
    let Ok(Ok(mut tcp)) = tokio::time::timeout(connect_timeout, TcpStream::connect(addr)).await else {
        return "reset";
    };
    timing.connected = Some(started.elapsed());
    let first: &[u8] = match KINDS[kind] {
        "idle" => b"",
        "slow-head" => &SLOW_HEAD[..1],
        "slow-body" => SLOW_BODY_HEAD,
        _ => NORMAL_REQUEST,
    };
    if !first.is_empty() && tcp.write_all(first).await.is_err() {
        return "reset";
    }
    let outq = OutQueue::of(&tcp);
    let acked = OnceLock::new();
    let watch = async {
        if !first.is_empty()
            && let Some(at) = outq.wait_empty(started).await
        {
            let _ = acked.set(at);
        }
        std::future::pending::<()>().await
    };
    let work = async {
        if KINDS[kind] == "slow-head" {
            let (mut rd, mut wr) = tcp.split();
            let trickle = async {
                for b in SLOW_HEAD[1..].iter().chain(std::iter::repeat(&b'a')) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    if wr.write_all(&[*b]).await.is_err() {
                        return;
                    }
                }
            };
            let mut got = Vec::new();
            let read = rd.read_to_end(&mut got);
            return tokio::select! {
                r = read => classify(r.map(|_| got)),
                _ = trickle => "other",
            };
        }
        let mut got = Vec::new();
        classify(tcp.read_to_end(&mut got).await.map(|_| got))
    };
    let outcome = tokio::select! {
        r = tokio::time::timeout(patience, work) => r.unwrap_or_else(|_| {
            timing.abandoned = true;
            "other"
        }),
        () = watch => unreachable!("the watch never ends"),
    };
    timing.acked = acked.get().copied();
    // A response can arrive between two looks at the queue; the bytes were
    // acknowledged by now at the latest.
    if !first.is_empty() && timing.acked.is_none() && outq.is_empty() == Some(true) {
        timing.acked = Some(started.elapsed());
    }
    outcome
}

/// Bytes written to a socket that its peer has not acknowledged yet.
#[derive(Clone, Copy)]
struct OutQueue {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::RawFd,
}

impl OutQueue {
    /// The caller keeps `tcp` open for as long as the result is used.
    fn of(tcp: &TcpStream) -> OutQueue {
        #[cfg(target_os = "linux")]
        return OutQueue { fd: std::os::fd::AsRawFd::as_raw_fd(tcp) };
        #[cfg(not(target_os = "linux"))]
        {
            let _ = tcp;
            OutQueue {}
        }
    }

    /// Whether every byte written was acknowledged; `None` if unknown.
    fn is_empty(self) -> Option<bool> {
        #[cfg(target_os = "linux")]
        {
            let mut n: libc::c_int = 0;
            // SAFETY: TIOCOUTQ writes one c_int, and the fd is open (see `of`).
            let r = unsafe { libc::ioctl(self.fd, libc::TIOCOUTQ, &mut n) };
            (r == 0).then_some(n == 0)
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// Wait until [`Self::is_empty`] and return when, from `started`. Looks
    /// every 2 ms at first, backing off to every 50 ms.
    async fn wait_empty(self, started: Instant) -> Option<Duration> {
        let mut pause = Duration::from_millis(2);
        loop {
            match self.is_empty() {
                Some(true) => return Some(started.elapsed()),
                Some(false) => {}
                None => return None,
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_millis(50));
        }
    }
}

fn classify(r: std::io::Result<Vec<u8>>) -> &'static str {
    match r {
        Ok(got) if got.is_empty() => "closed",
        Ok(got) if got.starts_with(b"HTTP/1.1 200") => "served",
        Ok(got) if got.starts_with(b"HTTP/1.1 408") => "timeout",
        // Ingest admission (429) or shutdown (503 from a handler).
        Ok(got) if got.starts_with(b"HTTP/1.1 429") => "rejected",
        Ok(got) if got.starts_with(BUSY_PREFIX) || BUSY_PREFIX.starts_with(&got) => "refused",
        Ok(_) => "other",
        Err(_) => "reset",
    }
}

struct Wave {
    took: Duration,
    outcomes: BTreeMap<String, u64>,
    timings: Vec<Timing>,
}

async fn wave(addr: SocketAddr, attempts: usize, connect_timeout: Duration, patience: Duration) -> Wave {
    let started = Instant::now();
    let tasks: Vec<_> = (0..attempts)
        .map(|i| (i % KINDS.len(), tokio::spawn(attempt(addr, i % KINDS.len(), connect_timeout, patience))))
        .collect();
    let mut outcomes = BTreeMap::new();
    for kind in KINDS {
        for outcome in OUTCOMES {
            outcomes.insert(format!("{kind}.{outcome}"), 0);
        }
    }
    let mut timings = Vec::with_capacity(attempts);
    for (kind, t) in tasks {
        let (outcome, timing) = t.await.unwrap_or(("other", Timing::default()));
        *outcomes.entry(format!("{}.{outcome}", KINDS[kind])).or_default() += 1;
        timings.push(timing);
    }
    Wave { took: started.elapsed(), outcomes, timings }
}

/// Connect, establishment, accept-to-end and end-to-end distributions of
/// `timings`.
fn timing_summary(timings: &[Timing]) -> Value {
    let connected: Vec<&Timing> = timings.iter().filter(|t| t.connected.is_some()).collect();
    let by_ack = connected.iter().filter(|t| t.acked.is_some()).count();
    let accept_to_end = connected.iter().filter_map(|t| t.accept_to_end());
    json!({
        "connect": distribution(connected.iter().filter_map(|t| t.connected).map(micros).collect()),
        "establishment": distribution(connected.iter().filter_map(|t| t.established()).map(micros).collect()),
        "acceptToEnd": distribution(accept_to_end.map(micros).collect()),
        "endToEnd": distribution(timings.iter().map(|t| micros(t.ended)).collect()),
        "establishedBy": { "firstBytesAcked": by_ack, "connectOnly": connected.len() - by_ack },
        "connectFailures": timings.len() - connected.len(),
        "abandoned": timings.iter().filter(|t| t.abandoned).count(),
    })
}

/// Every wave together: the distributions over all attempts, the longest
/// wave and the worst wave's figures.
fn flood_summary(waves: &[Value], timings: &[Timing]) -> Value {
    let worst = |path: &[&str]| -> Value {
        let at = |w: &Value| path.iter().fold(w, |v, k| &v[*k]).as_f64();
        waves.iter().filter_map(at).reduce(f64::max).map_or(Value::Null, |v| json!(v))
    };
    let mut v = timing_summary(timings);
    v["waves"] = json!(waves.len());
    v["longestWaveS"] = worst(&["tookS"]);
    v["worstWave"] = json!({
        "establishmentP99Ms": worst(&["establishment", "p99Ms"]),
        "establishmentMaxMs": worst(&["establishment", "maxMs"]),
        "acceptToEndMaxMs": worst(&["acceptToEnd", "maxMs"]),
        "endToEndMaxMs": worst(&["endToEnd", "maxMs"]),
    });
    v
}

const SAMPLE_FIELDS: [&str; 12] = [
    "active",
    "pending",
    "admitted",
    "queued",
    "refused",
    "headerTimeouts",
    "bodyTimeouts",
    "writeTimeouts",
    "protocolErrors",
    "acceptErrors",
    "ingestInFlight",
    "ingestRejected",
];

async fn sampler(
    args: Args,
    client: reqwest::Client,
    until: Instant,
    start: Instant,
) -> (String, BTreeMap<String, u64>) {
    let url = format!("{}/api/v1/system/info", args.target);
    let mut csv = format!("elapsedMs,ok,{}\n", SAMPLE_FIELDS.join(","));
    let mut peaks: BTreeMap<String, u64> = BTreeMap::new();
    let mut tick = tokio::time::interval(args.sample_interval);
    while Instant::now() < until {
        tick.tick().await;
        let elapsed = start.elapsed().as_millis();
        let v = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
            _ => None,
        };
        let Some(v) = v else {
            csv.push_str(&format!("{elapsed},0{}\n", ",".repeat(SAMPLE_FIELDS.len())));
            continue;
        };
        let c = &v["connections"];
        let row: Vec<u64> = SAMPLE_FIELDS
            .iter()
            .map(|f| match *f {
                "ingestInFlight" => v["ingest"]["inFlight"].as_u64().unwrap_or(0),
                "ingestRejected" => v["ingest"]["rejected"].as_u64().unwrap_or(0),
                f => c[f].as_u64().unwrap_or(0),
            })
            .collect();
        for (f, n) in SAMPLE_FIELDS.iter().zip(&row) {
            let peak = peaks.entry(f.to_string()).or_default();
            *peak = (*peak).max(*n);
        }
        csv.push_str(&format!("{elapsed},1,{}\n", row.iter().map(u64::to_string).collect::<Vec<_>>().join(",")));
    }
    (csv, peaks)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    // Resolved once: 1,000 simultaneous lookups through Docker's DNS lose
    // some, and glibc retries them after 5 s, inside `connect`.
    let host = host_port(&args.target)?;
    let addr = tokio::net::lookup_host(&host).await?.next().with_context(|| format!("{host} resolved to nothing"))?;
    std::fs::create_dir_all(&args.out).with_context(|| format!("creating {}", args.out.display()))?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
    let info: Value = client.get(format!("{}/api/v1/system/info", args.target)).send().await?.json().await?;
    let config: Value = client.get(format!("{}/api/v1/system/config", args.target)).send().await?.json().await?;

    let start = Instant::now();
    let until = start + args.duration;
    let steady = Arc::new(Steady::default());
    let mut load = Vec::new();
    for id in 0..args.ingest_clients {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
        load.push(tokio::spawn(ingest_sender(args.clone(), client, steady.clone(), until, id)));
    }
    for _ in 0..args.query_clients {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
        load.push(tokio::spawn(query_sender(args.clone(), client, steady.clone(), until)));
    }
    let sampling = tokio::spawn(sampler(args.clone(), client.clone(), until, start));

    let mut waves = Vec::new();
    let mut all_timings = Vec::new();
    if !args.no_waves {
        let mut next = start + Duration::from_secs(5);
        while next + Duration::from_secs(1) < until {
            tokio::time::sleep_until(next.into()).await;
            let at = start.elapsed();
            let w = wave(addr, args.attempts, args.connect_timeout, args.wave_patience).await;
            let mut entry = timing_summary(&w.timings);
            eprintln!(
                "connload: wave at {:.0}s took {:.1}s (establishment p99 {} ms, max {} ms; accept to end max {} ms): {:?}",
                at.as_secs_f64(),
                w.took.as_secs_f64(),
                entry["establishment"]["p99Ms"],
                entry["establishment"]["maxMs"],
                entry["acceptToEnd"]["maxMs"],
                w.outcomes
            );
            entry["startedAtS"] = json!(at.as_secs_f64());
            entry["tookS"] = json!(w.took.as_secs_f64());
            entry["outcomes"] = json!(w.outcomes);
            waves.push(entry);
            all_timings.extend(w.timings);
            next += args.wave_interval;
            while next < Instant::now() {
                next += args.wave_interval;
            }
        }
    }
    for t in load {
        t.await?;
    }
    let (csv, peaks) = sampling.await?;
    std::fs::write(args.out.join("samples.csv"), csv)?;

    // Acknowledged events must all be stored and queryable.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let persisted = client
        .post(format!("{}/api/v1/query/logs/count", args.target))
        .json(&json!({ "query": format!("service = connload-{}", args.label) }))
        .send()
        .await?
        .json::<Value>()
        .await?["count"]
        .as_u64();
    let after: Value = client.get(format!("{}/api/v1/system/info", args.target)).send().await?.json().await?;
    let acknowledged = steady.acknowledged.load(Ordering::Relaxed);
    let elapsed = start.elapsed().as_secs_f64();
    let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
    let summary = json!({
        "label": args.label,
        "startedAtMs": now_ms() - (elapsed * 1000.0) as u128,
        "durationS": elapsed,
        "args": {
            "attempts": args.attempts, "waveIntervalS": args.wave_interval.as_secs_f64(), "noWaves": args.no_waves,
            "connectTimeoutS": args.connect_timeout.as_secs_f64(), "wavePatienceS": args.wave_patience.as_secs_f64(),
            "ingestClients": args.ingest_clients, "batch": args.batch, "ingestIntervalMs": args.ingest_interval.as_millis() as u64,
            "queryClients": args.query_clients, "queryIntervalMs": args.query_interval.as_millis() as u64,
        },
        "server": { "version": info["version"], "config": config["config"]["server"], "before": info["connections"], "after": after["connections"] },
        "ingest": {
            "requestsOk": n(&steady.ingest_ok), "rejected429": n(&steady.ingest_429), "unavailable503": n(&steady.ingest_503),
            "otherStatus": n(&steady.ingest_other), "transportErrors": n(&steady.ingest_errors),
            "acknowledgedEvents": acknowledged, "persistedEvents": persisted,
            "eventsPerSecond": acknowledged as f64 / elapsed,
            "ackLatency": steady.ingest_latency.summary(),
        },
        "query": {
            "ok": n(&steady.query_ok), "unavailable503": n(&steady.query_503), "otherStatus": n(&steady.query_other),
            "transportErrors": n(&steady.query_errors), "latency": steady.query_latency.summary(),
        },
        "flood": if args.no_waves { Value::Null } else { flood_summary(&waves, &all_timings) },
        "waves": waves,
        "peaks": peaks,
        "reconciled": persisted == Some(acknowledged),
    });
    std::fs::write(args.out.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if persisted != Some(acknowledged) {
        anyhow::bail!("{acknowledged} events acknowledged but {persisted:?} stored");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn distribution_reports_percentiles_in_ms() {
        let v = distribution((1..=100).map(|n| n * 1000).collect());
        assert_eq!(v["samples"], 100);
        assert_eq!(
            (v["p50Ms"].as_f64(), v["p99Ms"].as_f64(), v["maxMs"].as_f64()),
            (Some(51.0), Some(99.0), Some(100.0))
        );
        assert_eq!(distribution(Vec::new())["p99Ms"], Value::Null);
    }

    /// The 0.1.2-rc.3 case: connected at once, but the server's kernel took
    /// 31 s to complete the handshake, then the body timeout took 60 s.
    #[test]
    fn establishment_and_accept_to_end_are_split() {
        let timings = [
            Timing { connected: Some(ms(1)), acked: Some(ms(31_000)), ended: ms(91_000), abandoned: false },
            Timing { connected: Some(ms(2)), acked: Some(ms(3)), ended: ms(60_003), abandoned: false },
            // Idle: nothing to acknowledge, so connect is the lower bound.
            Timing { connected: Some(ms(4)), acked: None, ended: ms(10_004), abandoned: false },
            // Idle, and the server never ended it: no establishment, no
            // accept-to-end, but it counts end to end.
            Timing { connected: Some(ms(5)), acked: None, ended: ms(95_000), abandoned: true },
            // Never connected.
            Timing { connected: None, acked: None, ended: ms(90_000), abandoned: false },
        ];
        let v = timing_summary(&timings);
        assert_eq!(v["establishment"]["maxMs"], 31_000.0);
        assert_eq!(v["establishment"]["samples"], 3);
        assert_eq!(v["connect"]["maxMs"], 5.0);
        assert_eq!(v["acceptToEnd"]["maxMs"], 60_000.0);
        assert_eq!(v["acceptToEnd"]["samples"], 3);
        assert_eq!(v["endToEnd"]["maxMs"], 95_000.0);
        assert_eq!(v["establishedBy"], json!({ "firstBytesAcked": 2, "connectOnly": 2 }));
        assert_eq!((v["connectFailures"].as_u64(), v["abandoned"].as_u64()), (Some(1), Some(1)));

        let waves = [json!({ "tookS": 91.0, "establishment": { "p99Ms": 31_000.0 } }), json!({ "tookS": 12.5 })];
        let f = flood_summary(&waves, &timings);
        assert_eq!((f["waves"].as_u64(), f["longestWaveS"].as_f64()), (Some(2), Some(91.0)));
        assert_eq!(f["worstWave"]["establishmentP99Ms"], 31_000.0);
        assert_eq!(f["worstWave"]["acceptToEndMaxMs"], Value::Null);
    }

    /// Answers each connection's request with 200 after `delay`, or closes
    /// idle ones after `delay`.
    async fn server(delay: Duration) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let got = tokio::time::timeout(delay, s.read(&mut buf)).await;
                    tokio::time::sleep(delay).await;
                    if matches!(got, Ok(Ok(n)) if n > 0) {
                        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn attempts_time_connect_acknowledgement_and_end() {
        let addr = server(ms(300)).await;
        let (outcome, t) = attempt(addr, 3, ms(5000), ms(5000)).await;
        assert_eq!(outcome, "served");
        let connected = t.connected.unwrap();
        assert!(connected < ms(250) && t.ended >= ms(300), "{t:?}");
        // The local kernel acknowledges at once; the response waits 300 ms.
        if cfg!(target_os = "linux") {
            assert!(t.acked.unwrap() < ms(250), "{t:?}");
        } else {
            assert_eq!(t.acked, None);
        }
        assert!(t.established().unwrap() <= t.ended);

        let (outcome, t) = attempt(addr, 0, ms(5000), ms(5000)).await;
        assert_eq!(outcome, "closed");
        assert_eq!((t.acked, t.established()), (None, t.connected));
        assert!(!t.abandoned);

        // The server holds it past the client's patience.
        let (outcome, t) = attempt(server(ms(5000)).await, 0, ms(5000), ms(300)).await;
        assert_eq!(outcome, "other");
        assert!(t.abandoned && t.established().is_none() && t.accept_to_end().is_none(), "{t:?}");

        // Nothing listening: a failed connect, not an establishment.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        let (outcome, t) = attempt(closed, 3, ms(5000), ms(5000)).await;
        assert_eq!((outcome, t.connected), ("reset", None));
    }
}
