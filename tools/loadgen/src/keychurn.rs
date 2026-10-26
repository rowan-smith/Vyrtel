//! Authenticated ingest under API key churn (AUTH-PERF-01).
//!
//! ```text
//! cargo run --release -p loadgen --bin keychurn -- --target http://127.0.0.1:8080 \
//!     --admin-password secret --duration 30s --concurrency 32 --batch 1 \
//!     --churn-interval 20ms --out results/high-small-churn
//! ```
//!
//! Logs in as the admin, creates one ingest key (so every ingest request is
//! authenticated) and, for `--duration`, runs:
//!
//! * `--concurrency` closed-loop producers, each sending `--batch` events
//!   per request with that key, back to back;
//! * an auth probe: a keyless ingest request every `--probe-interval`,
//!   answered 401 by the authentication layer alone, so its latency is the
//!   time to an authentication decision (including scheduler wait);
//! * a heartbeat: `GET /health` every `--probe-interval`, for runtime
//!   responsiveness;
//! * unless `--churn-interval` is 0, one churner that creates an ingest
//!   key, revokes it, and waits `--churn-interval`, back to back.
//!
//! At the end it counts the acknowledged events back with a query
//! (`persisted` must equal `acknowledged`) and writes `summary.json`.
//! Process CPU and memory are measured outside, by the script that starts
//! the server (`scripts/keychurn-bench.ps1`).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::Parser;
use serde_json::{Value, json};

#[derive(Parser, Clone)]
#[command(name = "keychurn", about = "Authenticated ingest under API key churn against Vyrtel")]
struct Args {
    /// Vyrtel base URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    target: String,
    /// The admin password (`auth.admin_password`).
    #[arg(long, env = "VYRTEL_AUTH_ADMIN_PASSWORD")]
    admin_password: String,
    /// The admin user name.
    #[arg(long, default_value = "admin")]
    admin_username: String,
    /// How long to run, e.g. 30s, 2m.
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    duration: Duration,
    /// Concurrent closed-loop producers.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
    /// Events per ingest request.
    #[arg(long, default_value_t = 1)]
    batch: usize,
    /// Pause between one churner's create+revoke pairs; 0 disables churn.
    #[arg(long, default_value = "0ms", value_parser = parse_duration)]
    churn_interval: Duration,
    /// Time between auth probes and between heartbeats.
    #[arg(long, default_value = "10ms", value_parser = parse_duration)]
    probe_interval: Duration,
    /// A request taking longer than this counts as a timeout.
    #[arg(long, default_value = "10s", value_parser = parse_duration)]
    timeout: Duration,
    /// Directory for summary.json.
    #[arg(long, default_value = "keychurn-out")]
    out: std::path::PathBuf,
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
    } else {
        (s, 1.0)
    };
    let v: f64 = num.parse().map_err(|_| format!("invalid duration '{s}'"))?;
    Ok(Duration::from_secs_f64(v * mult))
}

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

/// Responses by outcome.
#[derive(Default)]
struct Statuses {
    ok: AtomicU64,
    s401: AtomicU64,
    s429: AtomicU64,
    s5xx: AtomicU64,
    other: AtomicU64,
    timeouts: AtomicU64,
    errors: AtomicU64,
}

impl Statuses {
    fn count(&self, r: &Result<reqwest::Response, reqwest::Error>, ok: u16) {
        let c = match r {
            Ok(r) if r.status().as_u16() == ok => &self.ok,
            Ok(r) => match r.status().as_u16() {
                401 => &self.s401,
                429 => &self.s429,
                500..=599 => &self.s5xx,
                _ => &self.other,
            },
            Err(e) if e.is_timeout() => &self.timeouts,
            Err(_) => &self.errors,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }

    fn summary(&self) -> Value {
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        json!({
            "ok": n(&self.ok), "unauthorized401": n(&self.s401), "rejected429": n(&self.s429),
            "serverError5xx": n(&self.s5xx), "otherStatus": n(&self.other),
            "timeouts": n(&self.timeouts), "transportErrors": n(&self.errors),
        })
    }
}

#[derive(Default)]
struct Totals {
    acknowledged: AtomicU64,
    ingest: Statuses,
    ack_latency: Latencies,
    auth: Statuses,
    auth_latency: Latencies,
    health: Statuses,
    health_latency: Latencies,
    create: Statuses,
    create_latency: Latencies,
    revoke: Statuses,
    revoke_latency: Latencies,
}

async fn producer(args: Args, client: reqwest::Client, key: String, totals: Arc<Totals>, until: Instant, id: usize) {
    let url = format!("{}/api/v1/events", args.target);
    let service = format!("keychurn-{}", args.label);
    let mut seq = 0u64;
    while Instant::now() < until {
        let body: String = (0..args.batch)
            .map(|_| {
                seq += 1;
                format!(
                    "{{\"message\":\"keychurn {id}-{seq}\",\"service\":\"{service}\",\"level\":\"Information\",\"properties\":{{\"producer\":{id},\"seq\":{seq}}}}}\n"
                )
            })
            .collect();
        let started = Instant::now();
        let r =
            client.post(&url).bearer_auth(&key).header("content-type", "application/x-ndjson").body(body).send().await;
        totals.ingest.count(&r, 200);
        if let Ok(r) = r
            && r.status().as_u16() == 200
        {
            let v: Value = r.json().await.unwrap_or(Value::Null);
            totals.ack_latency.record(started.elapsed());
            totals.acknowledged.fetch_add(v["accepted"].as_u64().unwrap_or(0), Ordering::Relaxed);
        }
    }
}

/// Keyless ingest requests (401 from authentication) and `/health`.
async fn probes(args: Args, client: reqwest::Client, totals: Arc<Totals>, until: Instant) {
    let ingest = format!("{}/api/v1/events", args.target);
    let health = format!("{}/health", args.target);
    let mut tick = tokio::time::interval(args.probe_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while Instant::now() < until {
        tick.tick().await;
        let started = Instant::now();
        let r = client.post(&ingest).header("content-type", "application/x-ndjson").body("{}\n").send().await;
        totals.auth.count(&r, 401);
        if r.as_ref().is_ok_and(|r| r.status().as_u16() == 401) {
            totals.auth_latency.record(started.elapsed());
        }
        let started = Instant::now();
        let r = client.get(&health).send().await;
        totals.health.count(&r, 200);
        if r.as_ref().is_ok_and(|r| r.status().is_success()) {
            totals.health_latency.record(started.elapsed());
        }
    }
}

async fn churner(args: Args, admin: reqwest::Client, totals: Arc<Totals>, until: Instant) -> u64 {
    let mut pairs = 0;
    while Instant::now() < until {
        let started = Instant::now();
        let r = admin
            .post(format!("{}/api/v1/api-keys", args.target))
            .json(&json!({ "name": format!("keychurn-{pairs}"), "scope": "ingest" }))
            .send()
            .await;
        totals.create.count(&r, 201);
        let Ok(r) = r else { continue };
        if r.status().as_u16() != 201 {
            continue;
        }
        let id = r.json::<Value>().await.ok().and_then(|v| v["id"].as_i64());
        totals.create_latency.record(started.elapsed());
        let Some(id) = id else { continue };
        let started = Instant::now();
        let r = admin.delete(format!("{}/api/v1/api-keys/{id}", args.target)).send().await;
        totals.revoke.count(&r, 204);
        if r.as_ref().is_ok_and(|r| r.status().as_u16() == 204) {
            totals.revoke_latency.record(started.elapsed());
        }
        pairs += 1;
        tokio::time::sleep(args.churn_interval).await;
    }
    pairs
}

/// A client that sends the admin session cookie.
async fn login(args: &Args) -> anyhow::Result<reqwest::Client> {
    let r = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", args.target))
        .json(&json!({ "username": args.admin_username, "password": args.admin_password }))
        .send()
        .await
        .context("logging in")?;
    if !r.status().is_success() {
        bail!("login failed: {}", r.status());
    }
    let cookie = r.headers().get("set-cookie").context("no session cookie")?.to_str()?;
    let cookie = cookie.split(';').next().unwrap_or_default().to_string();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::COOKIE, cookie.parse()?);
    Ok(reqwest::Client::builder().default_headers(headers).timeout(args.timeout).build()?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out).with_context(|| format!("creating {}", args.out.display()))?;
    let admin = login(&args).await?;
    let info: Value = admin.get(format!("{}/api/v1/system/info", args.target)).send().await?.json().await?;
    let created: Value = admin
        .post(format!("{}/api/v1/api-keys", args.target))
        .json(&json!({ "name": format!("keychurn-producers-{}", args.label), "scope": "ingest" }))
        .send()
        .await?
        .json()
        .await?;
    let key = created["key"].as_str().context("creating the producers' key")?.to_string();

    let start = Instant::now();
    let until = start + args.duration;
    let totals = Arc::new(Totals::default());
    let mut load = Vec::new();
    for id in 0..args.concurrency {
        let client = reqwest::Client::builder().timeout(args.timeout).build()?;
        load.push(tokio::spawn(producer(args.clone(), client, key.clone(), totals.clone(), until, id)));
    }
    let client = reqwest::Client::builder().timeout(args.timeout).build()?;
    load.push(tokio::spawn(probes(args.clone(), client, totals.clone(), until)));
    let churn = (!args.churn_interval.is_zero())
        .then(|| tokio::spawn(churner(args.clone(), admin.clone(), totals.clone(), until)));
    for t in load {
        t.await?;
    }
    let pairs = match churn {
        Some(t) => t.await?,
        None => 0,
    };
    let elapsed = start.elapsed().as_secs_f64();

    // Acknowledged events must all be stored and queryable.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let persisted = admin
        .post(format!("{}/api/v1/query/logs/count", args.target))
        .json(&json!({ "query": format!("service = keychurn-{}", args.label) }))
        .send()
        .await?
        .json::<Value>()
        .await?["count"]
        .as_u64();
    let after: Value = admin.get(format!("{}/api/v1/system/info", args.target)).send().await?.json().await?;
    let t = &totals;
    let acknowledged = t.acknowledged.load(Ordering::Relaxed);
    let requests_ok = t.ingest.ok.load(Ordering::Relaxed);
    let summary = json!({
        "label": args.label,
        "durationS": elapsed,
        "args": {
            "concurrency": args.concurrency, "batch": args.batch,
            "churnIntervalMs": args.churn_interval.as_millis() as u64,
            "probeIntervalMs": args.probe_interval.as_millis() as u64,
            "timeoutMs": args.timeout.as_millis() as u64,
        },
        "server": { "version": info["version"], "rssBytesAfter": after["memory"]["rssBytes"] },
        "ingest": {
            "statuses": t.ingest.summary(),
            "acknowledgedEvents": acknowledged, "persistedEvents": persisted,
            "eventsPerSecond": acknowledged as f64 / elapsed,
            "requestsPerSecond": requests_ok as f64 / elapsed,
            "ackLatency": t.ack_latency.summary(),
        },
        "auth": { "statuses": t.auth.summary(), "latency": t.auth_latency.summary() },
        "heartbeat": { "statuses": t.health.summary(), "latency": t.health_latency.summary() },
        "churn": {
            "pairs": pairs,
            "create": { "statuses": t.create.summary(), "latency": t.create_latency.summary() },
            "revoke": { "statuses": t.revoke.summary(), "latency": t.revoke_latency.summary() },
        },
        "reconciled": persisted == Some(acknowledged),
    });
    std::fs::write(args.out.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if persisted != Some(acknowledged) {
        bail!("{acknowledged} events acknowledged but {persisted:?} stored");
    }
    Ok(())
}
