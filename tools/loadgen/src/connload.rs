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
//! `/api/v1/system/info` is sampled every `--sample-interval` into
//! `samples.csv`. At the end the events the server acknowledged are
//! counted back with a query (`persisted` must equal `acknowledged`), and
//! `summary.json` holds every total, latency percentile and peak.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// How long one wave's connections are given to end on their own.
    #[arg(long, default_value = "90s", value_parser = parse_duration)]
    wave_patience: Duration,
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

#[derive(Default)]
struct Latencies(Mutex<Vec<u32>>);

impl Latencies {
    fn record(&self, d: Duration) {
        self.0.lock().unwrap().push(d.as_micros().min(u32::MAX as u128) as u32);
    }

    fn summary(&self) -> Value {
        let mut v = self.0.lock().unwrap().clone();
        v.sort_unstable();
        let p = |q: f64| -> Value {
            if v.is_empty() {
                return Value::Null;
            }
            let i = ((v.len() as f64 - 1.0) * q).round() as usize;
            json!(v[i] as f64 / 1000.0)
        };
        json!({ "samples": v.len(), "p50Ms": p(0.50), "p95Ms": p(0.95), "p99Ms": p(0.99), "maxMs": p(1.0) })
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

/// One connection attempt of `kind`; returns its outcome.
async fn attempt(addr: String, kind: usize, patience: Duration) -> &'static str {
    let Ok(Ok(mut tcp)) = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&addr)).await else {
        return "reset";
    };
    let work = async {
        match KINDS[kind] {
            "idle" => {}
            "slow-head" => {
                let (mut rd, mut wr) = tcp.split();
                let trickle = async {
                    for b in
                        b"GET /health HTTP/1.1\r\nHost: connload\r\nX-Slow: ".iter().chain(std::iter::repeat(&b'a'))
                    {
                        if wr.write_all(&[*b]).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                };
                let mut got = Vec::new();
                let read = rd.read_to_end(&mut got);
                return tokio::select! {
                    r = read => classify(r.map(|_| got)),
                    _ = trickle => "other",
                };
            }
            "slow-body" => {
                let head = "POST /api/v1/events HTTP/1.1\r\nHost: connload\r\nContent-Type: application/json\r\nContent-Length: 100000\r\n\r\n[{\"message\"";
                if tcp.write_all(head.as_bytes()).await.is_err() {
                    return "reset";
                }
            }
            _ => {
                if tcp.write_all(b"GET /health HTTP/1.1\r\nHost: connload\r\nConnection: close\r\n\r\n").await.is_err()
                {
                    return "reset";
                }
            }
        }
        let mut got = Vec::new();
        classify(tcp.read_to_end(&mut got).await.map(|_| got))
    };
    tokio::time::timeout(patience, work).await.unwrap_or("other")
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

async fn wave(addr: String, attempts: usize, patience: Duration) -> (Duration, BTreeMap<String, u64>) {
    let started = Instant::now();
    let tasks: Vec<_> = (0..attempts)
        .map(|i| (i % KINDS.len(), tokio::spawn(attempt(addr.clone(), i % KINDS.len(), patience))))
        .collect();
    let mut outcomes = BTreeMap::new();
    for kind in KINDS {
        for outcome in OUTCOMES {
            outcomes.insert(format!("{kind}.{outcome}"), 0);
        }
    }
    for (kind, t) in tasks {
        let outcome = t.await.unwrap_or("other");
        *outcomes.entry(format!("{}.{outcome}", KINDS[kind])).or_default() += 1;
    }
    (started.elapsed(), outcomes)
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
    let addr = host_port(&args.target)?;
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
    if !args.no_waves {
        let mut next = start + Duration::from_secs(5);
        while next + Duration::from_secs(1) < until {
            tokio::time::sleep_until(next.into()).await;
            let at = start.elapsed();
            let (took, outcomes) = wave(addr.clone(), args.attempts, args.wave_patience).await;
            eprintln!("connload: wave at {:.0}s took {:.1}s: {outcomes:?}", at.as_secs_f64(), took.as_secs_f64());
            waves.push(json!({ "startedAtS": at.as_secs_f64(), "tookS": took.as_secs_f64(), "outcomes": outcomes }));
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
