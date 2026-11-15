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
//!
//! Queries and `system/*` need a credential: `--admin-password` (signs in
//! as `--admin-username` and sends the session cookie) or `--admin-key`
//! (`Authorization: Bearer`). With `--create-ingest-key`, connload creates
//! an ingest-scoped key with that credential before the load, so the
//! producers exercise protected ingestion. Secrets are read from the
//! environment as well as flags and never written to `summary.json`.
//! `--create-ingest-key` changes the server for good (ingestion then needs a
//! key), so it is meant for disposable benchmark instances. The `slow-body`
//! flood attempts send the same ingest key (`--slow-body-auth key`), so they
//! reach the body read and its timeout rather than being refused by
//! authentication; `--slow-body-auth none` tests that refusal instead.
//!
//! The run is judged as a whole, not only by the count: `summary.json`
//! `workloadValid` is false, with the reasons in `failures`, and connload
//! exits 1, unless the steady load really ran (events acknowledged, queries
//! answered, every `system/info` sample read with all its counters), its
//! clients saw no unexpected status (anything but 200, or 429 for ingest)
//! or transport error, the stored count equals the acknowledged events, no
//! flood attempt was abandoned or lost, and, when the flood's `slow-body`
//! attempts were authenticated, the server's body timeout fired (counted
//! from the snapshot after every wave ended; sampling also runs until
//! then). `ingest.eventsPerSecond` is over the steady load's own
//! window (`loadWindowS`); the final count's time is `reconcileDurationMs`,
//! bounded by `--reconcile-timeout`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use reqwest::header::{AUTHORIZATION, COOKIE, HeaderMap, HeaderName, HeaderValue};
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
    /// Label recorded in the summary and used to tag events (`connload-<label>`
    /// is the service): letters, digits, `.`, `_` and `-`. Unique per run by
    /// default, so a reused data directory does not add earlier events.
    #[arg(long, value_parser = parse_label, default_value_t = default_label())]
    label: String,
    /// How long the final count may take to match the acknowledged events.
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    reconcile_timeout: Duration,
    /// Whether flood `slow-body` attempts send the ingest key: `key` (when
    /// connload has one) exercises the body timeout; `none` exercises the
    /// refusal of unauthenticated ingestion.
    #[arg(long, value_enum, default_value_t = SlowBodyAuth::Key)]
    slow_body_auth: SlowBodyAuth,
    /// The admin user to sign in as with `--admin-password`.
    #[arg(long, default_value = "admin")]
    admin_username: String,
    /// The admin password (`auth.admin_password`): sign in and send the
    /// session cookie with queries and `system/*` requests.
    #[arg(long, env = "VYRTEL_AUTH_ADMIN_PASSWORD", hide_env_values = true, conflicts_with = "admin_key")]
    admin_password: Option<String>,
    /// An admin-scoped API key to send as `Authorization: Bearer` with
    /// queries and `system/*` requests, instead of signing in.
    #[arg(long, env = "CONNLOAD_ADMIN_KEY", hide_env_values = true)]
    admin_key: Option<String>,
    /// Create an ingest-scoped key (needs an admin credential) and send it
    /// with every ingest batch. Once it exists the server requires a key for
    /// ingestion (in the default `automatic` mode).
    #[arg(long, conflicts_with = "ingest_key")]
    create_ingest_key: bool,
    /// An existing ingest key to send with every ingest batch.
    #[arg(long, env = "CONNLOAD_INGEST_KEY", hide_env_values = true)]
    ingest_key: Option<String>,
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
enum SlowBodyAuth {
    Key,
    None,
}

fn default_label() -> String {
    format!("run-{}", now_ms())
}

/// Labels go into NDJSON and query strings unescaped, so only safe characters.
fn parse_label(s: &str) -> Result<String, String> {
    let ok =
        !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok { Ok(s.to_string()) } else { Err(format!("'{s}': use 1-64 letters, digits, '.', '_' or '-'")) }
}

/// How a client authenticates; the summary records only this kind, never
/// the secret.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Credential {
    None,
    Session,
    AdminKey,
    IngestKey,
}

impl Credential {
    fn name(self) -> &'static str {
        match self {
            Credential::None => "none",
            Credential::Session => "session",
            Credential::AdminKey => "adminKey",
            Credential::IngestKey => "ingestKey",
        }
    }
}

/// A client with a 60 s timeout that sends `header` (marked sensitive) with
/// every request, and no cookie store.
fn client_with(header: Option<(HeaderName, &str)>) -> anyhow::Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    if let Some((name, value)) = header {
        let mut value = HeaderValue::from_str(value).context("the credential is not a valid header value")?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(reqwest::Client::builder().default_headers(headers).timeout(Duration::from_secs(60)).build()?)
}

/// The session cookie (`name=value`) from signing in.
async fn login(target: &str, username: &str, password: &str) -> anyhow::Result<String> {
    let r = client_with(None)?
        .post(format!("{target}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await
        .context("signing in")?;
    anyhow::ensure!(r.status().is_success(), "signing in as {username} failed: {}", r.status());
    let cookie = r.headers().get("set-cookie").context("signing in returned no session cookie")?.to_str()?;
    Ok(cookie.split(';').next().unwrap_or_default().to_string())
}

/// The admin credential header, if any, and its kind.
async fn admin_credential(args: &Args) -> anyhow::Result<(Option<(HeaderName, String)>, Credential)> {
    if let Some(key) = &args.admin_key {
        return Ok((Some((AUTHORIZATION, format!("Bearer {key}"))), Credential::AdminKey));
    }
    if let Some(password) = &args.admin_password {
        let cookie = login(&args.target, &args.admin_username, password).await?;
        return Ok((Some((COOKIE, cookie)), Credential::Session));
    }
    Ok((None, Credential::None))
}

/// Sends `r`, requiring success, and returns the JSON body.
async fn json_ok(r: reqwest::RequestBuilder, what: &str) -> anyhow::Result<Value> {
    let r = r.send().await.with_context(|| what.to_string())?;
    let status = r.status();
    if !status.is_success() {
        let hint = if matches!(status.as_u16(), 401 | 403) { " (pass --admin-password or --admin-key)" } else { "" };
        anyhow::bail!("{what}: {status}{hint}");
    }
    r.json().await.with_context(|| what.to_string())
}

/// The query that matches connload's own events.
fn service_query(label: &str) -> String {
    format!("service = \"connload-{label}\"")
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
            Ok(r) => match r.status().as_u16() {
                200 => match r.json::<Value>().await.ok().and_then(|v| v["accepted"].as_u64()) {
                    Some(accepted) => {
                        steady.ingest_latency.record(started.elapsed());
                        steady.ingest_ok.fetch_add(1, Ordering::Relaxed);
                        steady.acknowledged.fetch_add(accepted, Ordering::Relaxed);
                    }
                    // Accepted, but how many is unknown: reconciliation would be wrong.
                    None => drop(steady.ingest_errors.fetch_add(1, Ordering::Relaxed)),
                },
                429 => drop(steady.ingest_429.fetch_add(1, Ordering::Relaxed)),
                503 => drop(steady.ingest_503.fetch_add(1, Ordering::Relaxed)),
                _ => drop(steady.ingest_other.fetch_add(1, Ordering::Relaxed)),
            },
            Err(_) => drop(steady.ingest_errors.fetch_add(1, Ordering::Relaxed)),
        }
        tokio::time::sleep(args.ingest_interval).await;
    }
}

async fn query_sender(args: Args, client: reqwest::Client, steady: Arc<Steady>, until: Instant) {
    let url = format!("{}/api/v1/query/logs/count", args.target);
    let body = json!({ "query": format!("{} and level = Information", service_query(&args.label)) });
    while Instant::now() < until {
        let started = Instant::now();
        match client.post(&url).json(&body).send().await {
            Ok(r) => match r.status().as_u16() {
                200 => match r.json::<Value>().await.ok().and_then(|v| v["count"].as_u64()) {
                    Some(_) => {
                        steady.query_latency.record(started.elapsed());
                        steady.query_ok.fetch_add(1, Ordering::Relaxed);
                    }
                    None => drop(steady.query_errors.fetch_add(1, Ordering::Relaxed)),
                },
                503 => drop(steady.query_503.fetch_add(1, Ordering::Relaxed)),
                _ => drop(steady.query_other.fetch_add(1, Ordering::Relaxed)),
            },
            Err(_) => drop(steady.query_errors.fetch_add(1, Ordering::Relaxed)),
        }
        tokio::time::sleep(args.query_interval).await;
    }
}

const SLOW_HEAD: &[u8] = b"GET /health HTTP/1.1\r\nHost: connload\r\nX-Slow: ";
/// A head announcing a 100 KB body, then its first 10 bytes; with `key`, it
/// is authorized like the steady producers, so the server reads the body.
fn slow_body_head(key: Option<&str>) -> Arc<[u8]> {
    let auth = key.map(|k| format!("Authorization: Bearer {k}\r\n")).unwrap_or_default();
    format!(
        "POST /api/v1/events HTTP/1.1\r\nHost: connload\r\n{auth}Content-Type: application/json\r\nContent-Length: 100000\r\n\r\n[{{\"message\""
    )
    .into_bytes()
    .into()
}
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
    slow_body: Arc<[u8]>,
    connect_timeout: Duration,
    patience: Duration,
) -> (&'static str, Timing) {
    let started = Instant::now();
    let mut timing = Timing::default();
    let outcome = run_attempt(addr, kind, &slow_body, connect_timeout, patience, started, &mut timing).await;
    timing.ended = started.elapsed();
    if let (Some(a), Some(c)) = (timing.acked, timing.connected) {
        timing.acked = Some(a.max(c).min(timing.ended));
    }
    (outcome, timing)
}

async fn run_attempt(
    addr: SocketAddr,
    kind: usize,
    slow_body: &[u8],
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
        "slow-body" => slow_body,
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
    /// Attempts whose task panicked or was cancelled: no outcome at all.
    failed_tasks: usize,
}

async fn wave(
    addr: SocketAddr,
    attempts: usize,
    slow_body: &Arc<[u8]>,
    connect_timeout: Duration,
    patience: Duration,
) -> Wave {
    let started = Instant::now();
    let tasks: Vec<_> = (0..attempts)
        .map(|i| {
            let kind = i % KINDS.len();
            (kind, tokio::spawn(attempt(addr, kind, slow_body.clone(), connect_timeout, patience)))
        })
        .collect();
    let mut outcomes = BTreeMap::new();
    for kind in KINDS {
        for outcome in OUTCOMES {
            outcomes.insert(format!("{kind}.{outcome}"), 0);
        }
    }
    let mut timings = Vec::with_capacity(attempts);
    let mut failed_tasks = 0;
    for (kind, t) in tasks {
        match t.await {
            Ok((outcome, timing)) => {
                *outcomes.entry(format!("{}.{outcome}", KINDS[kind])).or_default() += 1;
                timings.push(timing);
            }
            Err(_) => failed_tasks += 1,
        }
    }
    Wave { took: started.elapsed(), outcomes, timings, failed_tasks }
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

/// What the sampler collected.
struct Samples {
    csv: String,
    peaks: BTreeMap<String, u64>,
    ok: u64,
    /// Not read: a transport error, a non-200 or a body that is not JSON.
    failed: u64,
    /// Read, but without every field of `SAMPLE_FIELDS` as a number.
    malformed: u64,
    /// The fields missing from malformed samples.
    missing: std::collections::BTreeSet<String>,
}

/// The `SAMPLE_FIELDS` of a `system/info` response, in order; a field that
/// is missing or not a number is an error, never a zero.
fn sample_row(v: &Value) -> Result<Vec<u64>, Vec<String>> {
    let mut row = Vec::with_capacity(SAMPLE_FIELDS.len());
    let mut missing = Vec::new();
    for f in SAMPLE_FIELDS {
        let path = match f {
            "ingestInFlight" => ["ingest", "inFlight"],
            "ingestRejected" => ["ingest", "rejected"],
            f => ["connections", f],
        };
        match v[path[0]][path[1]].as_u64() {
            Some(n) => row.push(n),
            None => missing.push(path.join(".")),
        }
    }
    if missing.is_empty() { Ok(row) } else { Err(missing) }
}

/// Samples `system/info` every `--sample-interval` until `stop` turns true:
/// through the steady load and every flood wave's cleanup.
async fn sampler(
    args: Args,
    client: reqwest::Client,
    mut stop: tokio::sync::watch::Receiver<bool>,
    start: Instant,
) -> Samples {
    let url = format!("{}/api/v1/system/info", args.target);
    let mut csv = format!("elapsedMs,ok,{}\n", SAMPLE_FIELDS.join(","));
    let mut peaks: BTreeMap<String, u64> = BTreeMap::new();
    let (mut ok, mut failed, mut malformed) = (0, 0, 0);
    let mut missing = std::collections::BTreeSet::new();
    let mut tick = tokio::time::interval(args.sample_interval);
    while !*stop.borrow() {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.changed() => continue,
        }
        let elapsed = start.elapsed().as_millis();
        let v = match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
            _ => None,
        };
        let empty = format!("{elapsed},0{}\n", ",".repeat(SAMPLE_FIELDS.len()));
        let Some(v) = v else {
            failed += 1;
            csv.push_str(&empty);
            continue;
        };
        let row = match sample_row(&v) {
            Ok(row) => row,
            Err(m) => {
                malformed += 1;
                missing.extend(m);
                csv.push_str(&empty);
                continue;
            }
        };
        ok += 1;
        for (f, n) in SAMPLE_FIELDS.iter().zip(&row) {
            let peak = peaks.entry(f.to_string()).or_default();
            *peak = (*peak).max(*n);
        }
        csv.push_str(&format!("{elapsed},1,{}\n", row.iter().map(u64::to_string).collect::<Vec<_>>().join(",")));
    }
    Samples { csv, peaks, ok, failed, malformed, missing }
}

/// How far the server's body-timeout counter rose during the run: from the
/// snapshot before the load to the higher of the sampled peak and the
/// snapshot after every wave ended (the authoritative cumulative value; a
/// timeout can fire after the last sample).
fn body_timeouts_rise(before: u64, sampled_peak: u64, after: Option<u64>) -> u64 {
    sampled_peak.max(after.unwrap_or(0)).saturating_sub(before)
}

/// The final count: polls until the stored count of connload's service
/// equals `acknowledged`, for at most `wait` in all (each request is cut off
/// at the deadline). Returns the last count read, the error that kept it
/// from matching (if any) and how long it took.
async fn reconcile(
    client: &reqwest::Client,
    target: &str,
    label: &str,
    acknowledged: u64,
    wait: Duration,
) -> (Option<u64>, Option<String>, Duration) {
    let started = Instant::now();
    let deadline = started + wait;
    let body = json!({ "query": service_query(label) });
    let mut persisted = None;
    let mut error: Option<String> = None;
    loop {
        let count =
            json_ok(client.post(format!("{target}/api/v1/query/logs/count")).json(&body), "counting stored events");
        match tokio::time::timeout_at(deadline.into(), count).await {
            Ok(Ok(v)) => (persisted, error) = (v["count"].as_u64(), None),
            Ok(Err(e)) => error = Some(format!("{e:#}")),
            // Cut off at the deadline: report the last count read, else the
            // last refusal, else that nothing answered.
            Err(_) if persisted.is_some() => error = None,
            Err(_) => {
                error =
                    error.or_else(|| Some(format!("counting stored events: no answer within {} s", wait.as_secs_f64())))
            }
        }
        let now = Instant::now();
        if (persisted == Some(acknowledged) && error.is_none()) || now >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500).min(deadline - now)).await;
    }
    if error.is_none() && persisted != Some(acknowledged) {
        error =
            Some(format!("{acknowledged} events acknowledged but {persisted:?} stored after {} s", wait.as_secs_f64()));
    }
    (persisted, error, started.elapsed())
}

/// Everything that decides whether a run is valid evidence.
#[derive(Default)]
struct Evidence {
    ingest_clients: usize,
    query_clients: usize,
    acknowledged: u64,
    /// Ingest: 503, other non-200 (not 429), transport errors.
    ingest_failures: [u64; 3],
    query_ok: u64,
    /// Query: 503, other non-200, transport errors.
    query_failures: [u64; 3],
    samples_ok: u64,
    samples_failed: u64,
    samples_malformed: u64,
    /// Fields missing from malformed samples (or from the final snapshot).
    samples_missing: Vec<String>,
    after_read: bool,
    /// Flood attempts still open when the client gave up (the server never
    /// ended them), and attempts whose task failed.
    abandoned: u64,
    failed_tasks: u64,
    reconcile_error: Option<String>,
    /// `Some(n)`: authenticated (or open) slow-body attempts were sent and
    /// the server's body-timeout counter rose by `n`.
    body_timeouts: Option<u64>,
}

/// What the load ran: both kinds of steady client, or one.
fn workload(e: &Evidence) -> &'static str {
    match (e.ingest_clients > 0, e.query_clients > 0) {
        (true, true) => "mixed",
        (true, false) => "ingest-only",
        (false, true) => "query-only",
        (false, false) => "none",
    }
}

/// Why the run is not valid evidence; empty when it is.
fn failures(e: &Evidence) -> Vec<String> {
    let mut f = Vec::new();
    let names = ["503 responses", "unexpected statuses (not 200 or 429)", "transport or response errors"];
    if workload(e) == "none" {
        f.push("no steady clients".to_string());
    }
    if e.ingest_clients > 0 && e.acknowledged == 0 {
        f.push("no ingested events were acknowledged".to_string());
    }
    for (n, name) in e.ingest_failures.iter().zip(names) {
        if *n > 0 {
            f.push(format!("ingest: {n} {name}"));
        }
    }
    if e.query_clients > 0 && e.query_ok == 0 {
        f.push("no query was answered".to_string());
    }
    for (n, name) in e.query_failures.iter().zip(names) {
        if *n > 0 {
            f.push(format!("query: {n} {}", name.replace(" (not 200 or 429)", " (not 200)")));
        }
    }
    let taken = e.samples_ok + e.samples_failed + e.samples_malformed;
    if e.samples_ok == 0 || e.samples_failed > 0 {
        f.push(format!("system/info: {} of {taken} samples failed", e.samples_failed));
    }
    if e.samples_malformed > 0 || !e.samples_missing.is_empty() {
        f.push(format!(
            "system/info: {} of {taken} samples lacked required fields ({})",
            e.samples_malformed,
            e.samples_missing.join(", ")
        ));
    }
    if e.abandoned > 0 {
        f.push(format!(
            "flood: {} attempts abandoned (still open when connload stopped waiting; the server never ended them)",
            e.abandoned
        ));
    }
    if e.failed_tasks > 0 {
        f.push(format!("flood: {} attempts' tasks failed, so their outcome is unknown", e.failed_tasks));
    }
    if !e.after_read {
        f.push("system/info could not be read after the load".to_string());
    }
    if let Some(r) = &e.reconcile_error {
        f.push(format!("not reconciled: {r}"));
    }
    if e.body_timeouts == Some(0) {
        f.push("flood: authenticated slow-body attempts never reached the body timeout".to_string());
    }
    f
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    // Resolved once: 1,000 simultaneous lookups through Docker's DNS lose
    // some, and glibc retries them after 5 s, inside `connect`.
    let host = host_port(&args.target)?;
    let addr = tokio::net::lookup_host(&host).await?.next().with_context(|| format!("{host} resolved to nothing"))?;
    std::fs::create_dir_all(&args.out).with_context(|| format!("creating {}", args.out.display()))?;
    let (admin_header, admin_auth) = admin_credential(&args).await?;
    let admin_header = admin_header.as_ref().map(|(n, v)| (n.clone(), v.as_str()));
    // The admin client: setup, queries, system/* and the final count.
    let client = client_with(admin_header.clone())?;
    let info = json_ok(client.get(format!("{}/api/v1/system/info", args.target)), "reading system/info").await?;
    if let Err(missing) = sample_row(&info) {
        anyhow::bail!("reading system/info: missing or non-numeric {}", missing.join(", "));
    }
    let config = json_ok(client.get(format!("{}/api/v1/system/config", args.target)), "reading system/config").await?;
    let ingest_key = if args.create_ingest_key {
        anyhow::ensure!(admin_auth != Credential::None, "--create-ingest-key needs --admin-password or --admin-key");
        let created = json_ok(
            client
                .post(format!("{}/api/v1/api-keys", args.target))
                .json(&json!({ "name": format!("connload-{}", args.label), "scope": "ingest" })),
            "creating the ingest key",
        )
        .await?;
        Some(created["key"].as_str().context("the created key has no 'key'")?.to_string())
    } else {
        args.ingest_key.clone()
    };
    let ingest_auth = if ingest_key.is_some() { Credential::IngestKey } else { Credential::None };
    let ingest_header = ingest_key.as_ref().map(|k| format!("Bearer {k}"));
    let ingest_header = ingest_header.as_deref().map(|v| (AUTHORIZATION, v));
    let slow_body_key = if args.slow_body_auth == SlowBodyAuth::Key { ingest_key.as_deref() } else { None };
    let slow_body = slow_body_head(slow_body_key);
    eprintln!("connload: queries authenticate with {}, ingestion with {}", admin_auth.name(), ingest_auth.name());

    let start = Instant::now();
    let until = start + args.duration;
    let steady = Arc::new(Steady::default());
    let mut load = Vec::new();
    for id in 0..args.ingest_clients {
        let client = client_with(ingest_header.clone())?;
        load.push(tokio::spawn(ingest_sender(args.clone(), client, steady.clone(), until, id)));
    }
    for _ in 0..args.query_clients {
        let client = client_with(admin_header.clone())?;
        load.push(tokio::spawn(query_sender(args.clone(), client, steady.clone(), until)));
    }
    let (stop_sampling, sampling_stopped) = tokio::sync::watch::channel(false);
    let sampling = tokio::spawn(sampler(args.clone(), client.clone(), sampling_stopped, start));
    // When the steady load itself ended, apart from the waves' cleanup.
    let load_done = tokio::spawn(async move {
        for t in load {
            t.await?;
        }
        Ok::<_, tokio::task::JoinError>(Instant::now())
    });

    let mut waves = Vec::new();
    let mut all_timings = Vec::new();
    let mut failed_tasks = 0;
    if !args.no_waves {
        let mut next = start + Duration::from_secs(5);
        while next + Duration::from_secs(1) < until {
            tokio::time::sleep_until(next.into()).await;
            let at = start.elapsed();
            let w = wave(addr, args.attempts, &slow_body, args.connect_timeout, args.wave_patience).await;
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
            failed_tasks += w.failed_tasks as u64;
            next += args.wave_interval;
            while next < Instant::now() {
                next += args.wave_interval;
            }
        }
    }
    let load_window = load_done.await??.duration_since(start).as_secs_f64();
    // Every wave has ended (or been given up on): stop sampling now, not at
    // the steady load's deadline, so the flood's cleanup is sampled too.
    let _ = stop_sampling.send(true);
    let samples = sampling.await?;
    std::fs::write(args.out.join("samples.csv"), &samples.csv)?;

    // Acknowledged events must all be stored and queryable. The last batches
    // get up to 30 s to become visible; a count that never matches, or a
    // count query that is refused, leaves `reconciled` false.
    let acknowledged = steady.acknowledged.load(Ordering::Relaxed);
    let (persisted, reconcile_error, reconcile_took) =
        reconcile(&client, &args.target, &args.label, acknowledged, args.reconcile_timeout).await;
    let after = json_ok(client.get(format!("{}/api/v1/system/info", args.target)), "reading system/info").await.ok();
    let elapsed = start.elapsed().as_secs_f64();
    let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
    let peaks = samples.peaks;
    // Slow-body attempts reach the body read when they carry the key, or
    // when ingestion takes no key (the steady producers then have none).
    let slow_body_reads = !waves.is_empty()
        && args.attempts > 2
        && (slow_body_key.is_some() || (ingest_key.is_none() && args.slow_body_auth == SlowBodyAuth::Key));
    let body_timeouts_before = info["connections"]["bodyTimeouts"].as_u64().unwrap_or(0);
    let after_missing = after.as_ref().map(sample_row).and_then(Result::err).unwrap_or_default();
    let mut samples_missing: Vec<String> = samples.missing.iter().cloned().collect();
    samples_missing.extend(after_missing.iter().map(|m| format!("{m} (after the load)")));
    let evidence = Evidence {
        ingest_clients: args.ingest_clients,
        query_clients: args.query_clients,
        acknowledged,
        ingest_failures: [n(&steady.ingest_503), n(&steady.ingest_other), n(&steady.ingest_errors)],
        query_ok: n(&steady.query_ok),
        query_failures: [n(&steady.query_503), n(&steady.query_other), n(&steady.query_errors)],
        samples_ok: samples.ok,
        samples_failed: samples.failed,
        samples_malformed: samples.malformed,
        samples_missing,
        after_read: after.is_some(),
        abandoned: all_timings.iter().filter(|t| t.abandoned).count() as u64,
        failed_tasks,
        reconcile_error: reconcile_error.clone(),
        body_timeouts: slow_body_reads.then(|| {
            body_timeouts_rise(
                body_timeouts_before,
                peaks.get("bodyTimeouts").copied().unwrap_or(0),
                after.as_ref().and_then(|a| a["connections"]["bodyTimeouts"].as_u64()),
            )
        }),
    };
    let failures = failures(&evidence);
    let after = after.unwrap_or(Value::Null);
    let summary = json!({
        "label": args.label,
        "startedAtMs": now_ms() - (elapsed * 1000.0) as u128,
        "durationS": elapsed,
        "args": {
            "attempts": args.attempts, "waveIntervalS": args.wave_interval.as_secs_f64(), "noWaves": args.no_waves,
            "connectTimeoutS": args.connect_timeout.as_secs_f64(), "wavePatienceS": args.wave_patience.as_secs_f64(),
            "ingestClients": args.ingest_clients, "batch": args.batch, "ingestIntervalMs": args.ingest_interval.as_millis() as u64,
            "queryClients": args.query_clients, "queryIntervalMs": args.query_interval.as_millis() as u64,
            "reconcileTimeoutS": args.reconcile_timeout.as_secs_f64(),
            "slowBodyAuth": if slow_body_key.is_some() { "ingestKey" } else { "none" },
        },
        "workload": workload(&evidence),
        "workloadValid": failures.is_empty(),
        "failures": failures,
        "loadWindowS": load_window,
        "auth": { "queries": admin_auth.name(), "ingest": ingest_auth.name() },
        "server": { "version": info["version"], "config": config["config"]["server"], "before": info["connections"], "after": after["connections"] },
        "ingest": {
            "requestsOk": n(&steady.ingest_ok), "rejected429": n(&steady.ingest_429), "unavailable503": n(&steady.ingest_503),
            "otherStatus": n(&steady.ingest_other), "transportErrors": n(&steady.ingest_errors),
            "acknowledgedEvents": acknowledged, "persistedEvents": persisted,
            "eventsPerSecond": acknowledged as f64 / load_window,
            "ackLatency": steady.ingest_latency.summary(),
        },
        "query": {
            "ok": n(&steady.query_ok), "unavailable503": n(&steady.query_503), "otherStatus": n(&steady.query_other),
            "transportErrors": n(&steady.query_errors), "latency": steady.query_latency.summary(),
        },
        "flood": if args.no_waves { Value::Null } else { flood_summary(&waves, &all_timings) },
        "waves": waves,
        "peaks": peaks,
        "samples": { "ok": samples.ok, "failed": samples.failed, "malformed": samples.malformed },
        "reconciled": reconcile_error.is_none() && persisted == Some(acknowledged),
        "reconcileError": reconcile_error,
        "reconcileDurationMs": reconcile_took.as_millis() as u64,
    });
    let mut summary = summary;
    if summary["flood"].is_object() {
        // Rise of the server's body-timeout counter; null when slow-body
        // attempts were not expected to reach the body read.
        summary["flood"]["bodyTimeouts"] = json!(evidence.body_timeouts);
        summary["flood"]["failedTasks"] = json!(evidence.failed_tasks);
    }
    std::fs::write(args.out.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    let failures = summary["failures"].as_array().cloned().unwrap_or_default();
    if !failures.is_empty() {
        let list: Vec<_> = failures.iter().filter_map(Value::as_str).collect();
        anyhow::bail!("the run is not valid evidence:\n  - {}", list.join("\n  - "));
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
        let (outcome, t) = attempt(addr, 3, slow_body_head(None), ms(5000), ms(5000)).await;
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

        let (outcome, t) = attempt(addr, 0, slow_body_head(None), ms(5000), ms(5000)).await;
        assert_eq!(outcome, "closed");
        assert_eq!((t.acked, t.established()), (None, t.connected));
        assert!(!t.abandoned);

        // The server holds it past the client's patience.
        let (outcome, t) = attempt(server(ms(5000)).await, 0, slow_body_head(None), ms(5000), ms(300)).await;
        assert_eq!(outcome, "other");
        assert!(t.abandoned && t.established().is_none() && t.accept_to_end().is_none(), "{t:?}");

        // Nothing listening: a failed connect, not an establishment.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        let (outcome, t) = attempt(closed, 3, slow_body_head(None), ms(5000), ms(5000)).await;
        assert_eq!((outcome, t.connected), ("reset", None));
    }

    fn parse(a: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("connload").chain(a.iter().copied()))
    }

    #[test]
    fn service_query_quotes_the_label() {
        assert_eq!(service_query("steady-1"), "service = \"connload-steady-1\"");
    }

    #[test]
    fn summary_names_credentials_by_kind() {
        let names: Vec<_> = [Credential::None, Credential::Session, Credential::AdminKey, Credential::IngestKey]
            .map(Credential::name)
            .into();
        assert_eq!(names, ["none", "session", "adminKey", "ingestKey"]);
    }

    #[test]
    fn one_admin_credential_and_one_ingest_key_source() {
        assert!(parse(&["--admin-password", "p", "--admin-key", "k"]).is_err());
        assert!(parse(&["--create-ingest-key", "--ingest-key", "k"]).is_err());
        let a = parse(&["--admin-password", "p", "--create-ingest-key"]).unwrap();
        assert_eq!(a.admin_password.as_deref(), Some("p"));
        assert_eq!(a.admin_username, "admin");
        assert!(a.create_ingest_key);
    }

    #[test]
    fn slow_body_carries_the_ingest_key_only_when_given() {
        let with = String::from_utf8(slow_body_head(Some("k1")).to_vec()).unwrap();
        assert!(with.contains("\r\nAuthorization: Bearer k1\r\n") && with.ends_with("\r\n\r\n[{\"message\""));
        assert!(with.find("Authorization").unwrap() < with.find("\r\n\r\n").unwrap());
        let without = String::from_utf8(slow_body_head(None).to_vec()).unwrap();
        assert!(!without.contains("Authorization") && without.contains("Content-Length: 100000\r\n"));
    }

    #[test]
    fn labels_are_restricted_and_unique_by_default() {
        assert!(parse_label("steady-1.a_b").is_ok());
        for bad in ["", "a\"b", "a b", "a\\b", &"x".repeat(65)] {
            assert!(parse_label(bad).is_err(), "{bad}");
        }
        assert!(parse(&["--label", "x\"}"]).is_err());
        let a = parse(&[]).unwrap();
        assert!(a.label.starts_with("run-") && parse_label(&a.label).is_ok());
    }

    fn valid() -> Evidence {
        Evidence {
            ingest_clients: 4,
            query_clients: 2,
            acknowledged: 1000,
            query_ok: 10,
            samples_ok: 60,
            after_read: true,
            ..Evidence::default()
        }
    }

    #[test]
    fn a_real_mixed_run_is_valid() {
        let e = valid();
        assert_eq!((workload(&e), failures(&e)), ("mixed", Vec::<String>::new()));
        let e = Evidence { body_timeouts: Some(250), ..valid() };
        assert!(failures(&e).is_empty());
    }

    /// A wrong ingest key: every batch refused, nothing stored, 0 == 0.
    #[test]
    fn a_run_that_ingested_nothing_is_not_valid_even_if_the_counts_match() {
        let e = Evidence { acknowledged: 0, ingest_failures: [0, 4000, 0], ..valid() };
        let f = failures(&e);
        assert!(f.iter().any(|m| m.contains("no ingested events")), "{f:?}");
        assert!(f.iter().any(|m| m.contains("ingest: 4000 unexpected statuses")), "{f:?}");
    }

    #[test]
    fn steady_503s_refused_queries_and_failed_samples_are_failures() {
        let e = Evidence {
            ingest_failures: [3, 0, 0],
            query_ok: 0,
            query_failures: [0, 60, 1],
            samples_failed: 2,
            after_read: false,
            ..valid()
        };
        let f = failures(&e);
        for want in [
            "ingest: 3 503",
            "no query was answered",
            "query: 60 unexpected statuses (not 200)",
            "query: 1 transport",
            "2 of 62 samples failed",
            "after the load",
        ] {
            assert!(f.iter().any(|m| m.contains(want)), "{want}: {f:?}");
        }
        assert!(failures(&Evidence { samples_ok: 0, ..valid() }).iter().any(|m| m.contains("0 of 0")));
    }

    #[test]
    fn reconciliation_and_body_timeouts_are_required() {
        let f =
            failures(&Evidence { reconcile_error: Some("1000 acknowledged but Some(990) stored".into()), ..valid() });
        assert!(f[0].starts_with("not reconciled"), "{f:?}");
        let f = failures(&Evidence { body_timeouts: Some(0), ..valid() });
        assert!(f[0].contains("body timeout"), "{f:?}");
    }

    /// The body timeout fires after the steady load (and the last sample):
    /// the final snapshot still counts it.
    #[test]
    fn body_timeouts_count_the_final_snapshot() {
        assert_eq!(body_timeouts_rise(5, 5, Some(255)), 250);
        assert_eq!(body_timeouts_rise(5, 105, Some(105)), 100);
        // A sample can be higher than nothing read afterwards.
        assert_eq!(body_timeouts_rise(5, 105, None), 100);
        assert_eq!(body_timeouts_rise(5, 0, Some(5)), 0);
    }

    /// `--wave-patience` shorter than the server's cleanup: the client gave
    /// up, which must not pass as the server ending the connections.
    #[test]
    fn abandoned_or_failed_flood_attempts_invalidate_the_run() {
        let f = failures(&Evidence { abandoned: 12, body_timeouts: Some(238), ..valid() });
        assert!(f.len() == 1 && f[0].contains("12 attempts abandoned"), "{f:?}");
        let f = failures(&Evidence { failed_tasks: 1, ..valid() });
        assert!(f.len() == 1 && f[0].contains("1 attempts' tasks failed"), "{f:?}");
    }

    fn info_sample() -> Value {
        let connections: serde_json::Map<String, Value> =
            SAMPLE_FIELDS[..10].iter().enumerate().map(|(i, f)| (f.to_string(), json!(i))).collect();
        json!({ "connections": connections, "ingest": { "inFlight": 10, "rejected": 11 } })
    }

    #[test]
    fn samples_need_every_field_as_a_number() {
        assert_eq!(sample_row(&info_sample()).unwrap(), (0..12).collect::<Vec<u64>>());
        let mut v = info_sample();
        v["connections"].as_object_mut().unwrap().remove("acceptErrors");
        v["ingest"]["rejected"] = json!("11");
        assert_eq!(sample_row(&v).unwrap_err(), ["connections.acceptErrors", "ingest.rejected"]);
        assert!(sample_row(&json!({})).unwrap_err().len() == SAMPLE_FIELDS.len());

        let f = failures(&Evidence {
            samples_malformed: 3,
            samples_missing: vec!["connections.acceptErrors".into()],
            ..valid()
        });
        assert!(
            f.len() == 1 && f[0].contains("3 of 63 samples lacked required fields (connections.acceptErrors)"),
            "{f:?}"
        );
        let f = failures(&Evidence { samples_missing: vec!["connections.refused (after the load)".into()], ..valid() });
        assert!(f.len() == 1 && f[0].contains("after the load"), "{f:?}");
    }

    /// Sampling continues past the steady load until told to stop, and a
    /// sample without the required fields counts as malformed, not zero.
    #[tokio::test]
    async fn the_sampler_runs_until_stopped_and_rejects_incomplete_samples() {
        let body = info_sample().to_string();
        let ok: &'static str = Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n{body}",
                body.len()
            )
            .into_boxed_str(),
        );
        let (target, requests) = stub(ok).await;
        let mut args = parse(&["--target", &target, "--sample-interval", "50ms", "--duration", "1ms"]).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let start = Instant::now();
        let run = tokio::spawn(sampler(args.clone(), client_with(None).unwrap(), stopped, start));
        tokio::time::sleep(ms(400)).await;
        stop.send(true).unwrap();
        let samples = run.await.unwrap();
        assert!(samples.ok >= 4 && samples.failed == 0 && samples.malformed == 0, "{} ok", samples.ok);
        assert_eq!(samples.ok, requests.load(Ordering::Relaxed));
        assert_eq!(samples.peaks["ingestRejected"], 11);

        let (target, _) =
            stub("HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: application/json\r\n\r\n{}").await;
        args.target = target;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let run = tokio::spawn(sampler(args, client_with(None).unwrap(), stopped, start));
        tokio::time::sleep(ms(200)).await;
        stop.send(true).unwrap();
        let samples = run.await.unwrap();
        assert!(samples.ok == 0 && samples.malformed >= 1, "{} malformed", samples.malformed);
        assert!(samples.missing.contains("connections.acceptErrors"));
    }

    #[test]
    fn an_ingest_only_run_is_labelled_and_needs_no_queries() {
        let e = Evidence { query_clients: 0, query_ok: 0, ..valid() };
        assert_eq!((workload(&e), failures(&e).len()), ("ingest-only", 0));
    }

    /// Answers every request with `response` (after reading its head and
    /// body), and counts the requests.
    async fn stub(response: &'static str) -> (String, Arc<AtomicU64>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicU64::new(0));
        let counter = requests.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut got = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        let Ok(n) = s.read(&mut buf).await else { return };
                        if n == 0 {
                            return;
                        }
                        got.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&got).to_ascii_lowercase();
                        let Some(end) = text.find("\r\n\r\n") else { continue };
                        let len = text[..end]
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if got.len() < end + 4 + len {
                            continue;
                        }
                        got.drain(..end + 4 + len);
                        counter.fetch_add(1, Ordering::Relaxed);
                        if s.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (target, requests)
    }

    const UNAUTHORIZED: &str =
        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 2\r\ncontent-type: application/json\r\n\r\n{}";

    #[tokio::test]
    async fn a_refused_sign_in_or_setup_request_fails_clearly() {
        let (target, _) = stub(UNAUTHORIZED).await;
        let e = login(&target, "admin", "wrong").await.unwrap_err();
        assert!(format!("{e:#}").contains("401"), "{e:#}");
        let client = client_with(None).unwrap();
        let e = json_ok(client.get(format!("{target}/api/v1/system/info")), "reading system/info").await.unwrap_err();
        assert!(format!("{e:#}").contains("--admin-password or --admin-key"), "{e:#}");
    }

    #[tokio::test]
    async fn reconciliation_matches_or_reports_why_not() {
        let client = client_with(None).unwrap();
        let (target, requests) =
            stub("HTTP/1.1 200 OK\r\ncontent-length: 14\r\ncontent-type: application/json\r\n\r\n{\"count\":1000}")
                .await;
        let (persisted, error, _) = reconcile(&client, &target, "t", 1000, ms(5000)).await;
        assert_eq!((persisted, error, requests.load(Ordering::Relaxed)), (Some(1000), None, 1));

        // A count that never matches: retried until the deadline, then reported.
        let (persisted, error, took) = reconcile(&client, &target, "t", 1001, ms(1200)).await;
        assert_eq!(persisted, Some(1000));
        assert!(error.unwrap().contains("1001 events acknowledged but Some(1000) stored"));
        assert!(took >= ms(1200) && took < ms(3000), "{took:?}");
        assert!(requests.load(Ordering::Relaxed) >= 3);

        let (target, _) = stub(UNAUTHORIZED).await;
        let (persisted, error, _) = reconcile(&client, &target, "t", 0, ms(300)).await;
        assert_eq!(persisted, None);
        assert!(error.unwrap().contains("401"));
    }

    /// The count request never answers: the deadline still holds, although
    /// the client's own timeout is 60 s.
    #[tokio::test]
    async fn reconciliation_stops_at_its_deadline_when_the_server_stalls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                held.push(s);
            }
        });
        let client = client_with(None).unwrap();
        let (persisted, error, took) = reconcile(&client, &target, "t", 5, ms(500)).await;
        assert_eq!(persisted, None);
        assert!(error.unwrap().contains("no answer within 0.5 s"));
        assert!(took >= ms(500) && took < ms(2000), "{took:?}");
    }

    #[test]
    fn credential_headers_are_validated() {
        assert!(client_with(Some((AUTHORIZATION, "Bearer abc"))).is_ok());
        assert!(client_with(Some((AUTHORIZATION, "bad\nvalue"))).is_err());
    }
}
