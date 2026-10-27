//! Session-cookie against admin-key authentication under the same load
//! (AUTH-PERF-02).
//!
//! ```text
//! cargo run --release -p loadgen --bin sessionauth -- --target http://127.0.0.1:8080 \
//!     --admin-password secret --auth session --workload mixed --concurrency 32 \
//!     --duration 20s --out results/mixed-high-session
//! ```
//!
//! Setup, with its own client: logs in as the admin, creates an admin key
//! and an ingest key, ingests `--seed-events` events and waits until they
//! are queryable, and creates a dashboard with one log-count panel.
//!
//! The load then uses exactly one credential (`--auth`): the session cookie
//! alone, or the admin key alone (`Authorization: Bearer`). Each load
//! client is built with only that header and no cookie store; before the
//! run, `GET /api/v1/auth/me` checks that the key clients carry no session
//! and the session clients do. For `--duration` it runs:
//!
//! * `--concurrency` closed-loop clients doing `--workload`:
//!   - `auth`: `GET /api/v1/system/config`, which reads no SQLite and runs
//!     no query, so its latency is authentication plus routing;
//!   - `query`: in turn a search (`/query/logs`, 50 events), facets
//!     (`/query/logs/facets`, `level` and `service`) and a dashboard view
//!     (`GET /dashboards/{id}`, then its panel's histogram);
//!   - `mixed`: `query`, plus `--ingest-concurrency` producers sending
//!     `--batch` events per request with the ingest key, and one writer
//!     creating and deleting a saved query every `--write-interval` with the
//!     load credential;
//! * an auth probe with the load credential: `GET /api/v1/system/config`
//!   every `--probe-interval`, for the authentication decision under load
//!   apart from query work;
//! * a `/health` heartbeat every `--probe-interval`.
//!
//! Requests in the first `--cold` of the run are reported separately
//! (`cold`) from the rest (`warm`). The server's session counters
//! (`sessionAuth` in `/api/v1/system/info`: cache hits, SQLite lookups and
//! their queue, lock and query time) are read before and after the run. At the end, in `mixed`, the acknowledged events are counted back
//! with a query (`persisted` must equal `acknowledged`). Writes
//! `summary.json`; process CPU and memory are measured outside, by
//! `scripts/bench/sessionauth.sh`. Credentials are never written out.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail, ensure};
use clap::{Parser, ValueEnum};
use reqwest::header::{AUTHORIZATION, COOKIE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Auth {
    Session,
    Key,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Workload {
    Auth,
    Query,
    Mixed,
}

#[derive(Parser, Clone)]
#[command(name = "sessionauth", about = "Session-cookie vs admin-key authentication under load against Vyrtel")]
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
    /// The only credential the load sends.
    #[arg(long, value_enum)]
    auth: Auth,
    /// What the closed-loop clients do.
    #[arg(long, value_enum, default_value = "auth")]
    workload: Workload,
    /// How long to run, e.g. 30s, 2m.
    #[arg(long, default_value = "20s", value_parser = parse_duration)]
    duration: Duration,
    /// Requests in this first part of the run are reported as `cold`.
    #[arg(long, default_value = "3s", value_parser = parse_duration)]
    cold: Duration,
    /// Concurrent closed-loop clients.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
    /// Events ingested (and made queryable) before the run.
    #[arg(long, default_value_t = 20_000)]
    seed_events: usize,
    /// Producers ingesting during a `mixed` run.
    #[arg(long, default_value_t = 4)]
    ingest_concurrency: usize,
    /// Events per ingest request (seeding and `mixed`).
    #[arg(long, default_value_t = 100)]
    batch: usize,
    /// Pause between saved-query create+delete pairs in a `mixed` run.
    #[arg(long, default_value = "50ms", value_parser = parse_duration)]
    write_interval: Duration,
    /// Time between auth probes and between heartbeats.
    #[arg(long, default_value = "10ms", value_parser = parse_duration)]
    probe_interval: Duration,
    /// A request taking longer than this counts as a timeout.
    #[arg(long, default_value = "10s", value_parser = parse_duration)]
    timeout: Duration,
    /// Directory for summary.json.
    #[arg(long, default_value = "sessionauth-out")]
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
    s403: AtomicU64,
    s429: AtomicU64,
    s503: AtomicU64,
    s5xx: AtomicU64,
    other: AtomicU64,
    timeouts: AtomicU64,
    errors: AtomicU64,
}

impl Statuses {
    fn count(&self, r: &Result<reqwest::Response, reqwest::Error>) -> bool {
        let c = match r {
            Ok(r) if r.status().is_success() => &self.ok,
            Ok(r) => match r.status().as_u16() {
                401 => &self.s401,
                403 => &self.s403,
                429 => &self.s429,
                503 => &self.s503,
                500..=599 => &self.s5xx,
                _ => &self.other,
            },
            Err(e) if e.is_timeout() => &self.timeouts,
            Err(_) => &self.errors,
        };
        c.fetch_add(1, Ordering::Relaxed);
        std::ptr::eq(c, &self.ok)
    }

    fn summary(&self) -> Value {
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        json!({
            "ok": n(&self.ok), "unauthorized401": n(&self.s401), "forbidden403": n(&self.s403),
            "rejected429": n(&self.s429), "unavailable503": n(&self.s503), "serverError5xx": n(&self.s5xx),
            "otherStatus": n(&self.other), "timeouts": n(&self.timeouts), "transportErrors": n(&self.errors),
        })
    }
}

/// One kind of request: outcomes, and latency of successes before and
/// after `--cold`.
#[derive(Default)]
struct Op {
    statuses: Statuses,
    cold: Latencies,
    warm: Latencies,
}

impl Op {
    /// Count `r`; record its latency when it succeeded. Returns success.
    fn done(&self, r: &Result<reqwest::Response, reqwest::Error>, started: Instant, warm_from: Instant) -> bool {
        let ok = self.statuses.count(r);
        if ok {
            let into = if started >= warm_from { &self.warm } else { &self.cold };
            into.record(started.elapsed());
        }
        ok
    }

    fn summary(&self) -> Value {
        json!({ "statuses": self.statuses.summary(), "cold": self.cold.summary(), "warm": self.warm.summary() })
    }
}

#[derive(Default)]
struct Totals {
    /// Every closed-loop request (all kinds together).
    load: Op,
    config: Op,
    search: Op,
    facets: Op,
    dashboard: Op,
    probe: Op,
    heartbeat: Op,
    ingest: Op,
    acknowledged: AtomicU64,
    writes: Op,
}

#[derive(Clone)]
struct Run {
    args: Args,
    service: String,
    dashboard: i64,
    until: Instant,
    warm_from: Instant,
    totals: Arc<Totals>,
}

impl Run {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.args.target)
    }

    fn query(&self) -> String {
        format!("service = \"{}\"", self.service)
    }
}

/// One closed-loop client: `--workload` back to back.
async fn client(run: Run, http: reqwest::Client, id: usize) {
    let t = &run.totals;
    let mut n = id;
    while Instant::now() < run.until {
        let started = Instant::now();
        let (op, r) = match (run.args.workload, n % 3) {
            (Workload::Auth, _) => (&t.config, http.get(run.url("/api/v1/system/config")).send().await),
            (_, 0) => {
                let body = json!({ "query": run.query(), "from": "-1h", "limit": 50 });
                (&t.search, http.post(run.url("/api/v1/query/logs")).json(&body).send().await)
            }
            (_, 1) => {
                let body =
                    json!({ "query": run.query(), "from": "-1h", "sample": 2000, "fields": ["level", "service"] });
                (&t.facets, http.post(run.url("/api/v1/query/logs/facets")).json(&body).send().await)
            }
            _ => (&t.dashboard, dashboard_view(&run, &http).await),
        };
        n += 1;
        let ok = op.done(&r, started, run.warm_from);
        t.load.done(&r, started, run.warm_from);
        if let Ok(r) = r {
            // Read the body, so a request ends when the client has it all.
            let _ = r.bytes().await;
        }
        if !ok {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// What the UI does to show a dashboard: load it, then run its panel.
async fn dashboard_view(run: &Run, http: &reqwest::Client) -> Result<reqwest::Response, reqwest::Error> {
    let r = http.get(run.url(&format!("/api/v1/dashboards/{}", run.dashboard))).send().await?;
    if !r.status().is_success() {
        return Ok(r);
    }
    let d: Value = r.json().await?;
    let query = d["panels"][0]["config"]["query"].as_str().unwrap_or_default().to_string();
    let body = json!({ "query": query, "from": "-1h", "buckets": 60 });
    http.post(run.url("/api/v1/query/logs/histogram")).json(&body).send().await
}

fn events(service: &str, producer: usize, seq: &mut u64, n: usize) -> String {
    (0..n)
        .map(|i| {
            *seq += 1;
            let level = ["Information", "Warning", "Error", "Debug"][(*seq as usize + i) % 4];
            format!(
                "{{\"message\":\"sessionauth {producer}-{seq}\",\"service\":\"{service}\",\"level\":\"{level}\",\"properties\":{{\"producer\":{producer},\"seq\":{seq},\"customerId\":\"c{}\"}}}}\n",
                *seq % 997
            )
        })
        .collect()
}

/// A `mixed` producer, with the ingest key.
async fn producer(run: Run, http: reqwest::Client, key: String, id: usize) {
    let t = &run.totals;
    let mut seq = 0;
    while Instant::now() < run.until {
        let body = events(&run.service, 1000 + id, &mut seq, run.args.batch);
        let started = Instant::now();
        let r = http
            .post(run.url("/api/v1/events"))
            .bearer_auth(&key)
            .header("content-type", "application/x-ndjson")
            .body(body)
            .send()
            .await;
        if t.ingest.done(&r, started, run.warm_from) {
            let v: Value = r.unwrap().json().await.unwrap_or(Value::Null);
            t.acknowledged.fetch_add(v["accepted"].as_u64().unwrap_or(0), Ordering::Relaxed);
        } else {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// A `mixed` metadata writer, with the load credential.
async fn writer(run: Run, http: reqwest::Client) -> u64 {
    let t = &run.totals;
    let mut pairs = 0;
    while Instant::now() < run.until {
        let started = Instant::now();
        let body = json!({ "name": format!("sessionauth-{pairs}"), "signal": "logs", "query": run.query() });
        let r = http.post(run.url("/api/v1/saved-queries")).json(&body).send().await;
        if t.writes.done(&r, started, run.warm_from)
            && let Some(id) = r.unwrap().json::<Value>().await.ok().and_then(|v| v["id"].as_i64())
        {
            let started = Instant::now();
            let r = http.delete(run.url(&format!("/api/v1/saved-queries/{id}"))).send().await;
            t.writes.done(&r, started, run.warm_from);
            pairs += 1;
        }
        tokio::time::sleep(run.args.write_interval).await;
    }
    pairs
}

/// The auth probe and the heartbeat.
async fn probes(run: Run, http: reqwest::Client, plain: reqwest::Client) {
    let t = &run.totals;
    let mut tick = tokio::time::interval(run.args.probe_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while Instant::now() < run.until {
        tick.tick().await;
        let started = Instant::now();
        let r = http.get(run.url("/api/v1/system/config")).send().await;
        t.probe.done(&r, started, run.warm_from);
        let started = Instant::now();
        let r = plain.get(run.url("/health")).send().await;
        t.heartbeat.done(&r, started, run.warm_from);
    }
}

/// The session cookie (`name=value`) from logging in.
async fn login(args: &Args) -> anyhow::Result<String> {
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
    Ok(cookie.split(';').next().unwrap_or_default().to_string())
}

/// A client sending only `header` (no cookie store, so nothing else).
fn client_with(header: (reqwest::header::HeaderName, &str), timeout: Duration) -> anyhow::Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    let mut value = HeaderValue::from_str(header.1)?;
    value.set_sensitive(true);
    headers.insert(header.0, value);
    Ok(reqwest::Client::builder().default_headers(headers).timeout(timeout).build()?)
}

async fn json_ok(r: reqwest::RequestBuilder, what: &str) -> anyhow::Result<Value> {
    let r = r.send().await.with_context(|| what.to_string())?;
    ensure!(r.status().is_success(), "{what}: {}", r.status());
    Ok(r.json().await.unwrap_or(Value::Null))
}

async fn count(admin: &reqwest::Client, args: &Args, query: &str) -> anyhow::Result<Option<u64>> {
    let v = json_ok(
        admin.post(format!("{}/api/v1/query/logs/count", args.target)).json(&json!({ "query": query, "from": "-1d" })),
        "counting events",
    )
    .await?;
    Ok(v["count"].as_u64())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out).with_context(|| format!("creating {}", args.out.display()))?;
    let base = |p: &str| format!("{}{p}", args.target);

    // Setup, with a session client of its own.
    let cookie = login(&args).await?;
    let admin = client_with((COOKIE, &cookie), args.timeout)?;
    let key_for = |scope: &str| {
        admin
            .post(base("/api/v1/api-keys"))
            .json(&json!({ "name": format!("sessionauth-{scope}-{}", args.label), "scope": scope }))
    };
    let admin_key =
        json_ok(key_for("admin"), "creating the admin key").await?["key"].as_str().context("no admin key")?.to_string();
    let ingest_key = json_ok(key_for("ingest"), "creating the ingest key").await?["key"]
        .as_str()
        .context("no ingest key")?
        .to_string();
    let service = format!("sessionauth-{}", args.label);
    let query = format!("service = \"{service}\"");
    let plain = reqwest::Client::builder().timeout(args.timeout).build()?;
    let mut seq = 0;
    let mut seeded = 0;
    while seeded < args.seed_events {
        let n = args.batch.min(args.seed_events - seeded);
        json_ok(
            plain
                .post(base("/api/v1/events"))
                .bearer_auth(&ingest_key)
                .header("content-type", "application/x-ndjson")
                .body(events(&service, 0, &mut seq, n)),
            "seeding events",
        )
        .await?;
        seeded += n;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while count(&admin, &args, &query).await? != Some(seeded as u64) {
        ensure!(Instant::now() < deadline, "seeded events not queryable after 30 s");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let dashboard = json_ok(
        admin.post(base("/api/v1/dashboards")).json(&json!({ "name": format!("sessionauth-{}", args.label) })),
        "creating the dashboard",
    )
    .await?["id"]
        .as_i64()
        .context("no dashboard id")?;
    json_ok(
        admin.post(base(&format!("/api/v1/dashboards/{dashboard}/panels"))).json(&json!({
            "title": "events", "kind": "log_count", "config": { "query": query }
        })),
        "adding the panel",
    )
    .await?;

    // The load's one credential; check that it is the only one sent.
    let load_header = match args.auth {
        Auth::Session => (COOKIE, cookie.clone()),
        Auth::Key => (AUTHORIZATION, format!("Bearer {admin_key}")),
    };
    let load = || client_with((load_header.0.clone(), &load_header.1), args.timeout);
    let me = json_ok(load()?.get(base("/api/v1/auth/me")), "checking the load credential").await?;
    match args.auth {
        Auth::Session => ensure!(me["user"] == json!(args.admin_username), "the session client has no session"),
        Auth::Key => ensure!(me["user"].is_null(), "the key client sends a session cookie"),
    }
    json_ok(load()?.get(base("/api/v1/system/config")), "authenticating with the load credential").await?;

    // Read with the key, so reading the counters looks up no session.
    let observer = client_with((AUTHORIZATION, &format!("Bearer {admin_key}")), args.timeout)?;
    let info_before = json_ok(observer.get(base("/api/v1/system/info")), "reading system info").await?;
    let start = Instant::now();
    let run = Run {
        service,
        dashboard,
        until: start + args.duration,
        warm_from: start + args.cold,
        totals: Arc::new(Totals::default()),
        args: args.clone(),
    };
    let mut tasks = Vec::new();
    for id in 0..args.concurrency {
        tasks.push(tokio::spawn(client(run.clone(), load()?, id)));
    }
    tasks.push(tokio::spawn(probes(run.clone(), load()?, plain.clone())));
    let mut writes = None;
    if args.workload == Workload::Mixed {
        for id in 0..args.ingest_concurrency {
            let http = reqwest::Client::builder().timeout(args.timeout).build()?;
            tasks.push(tokio::spawn(producer(run.clone(), http, ingest_key.clone(), id)));
        }
        writes = Some(tokio::spawn(writer(run.clone(), load()?)));
    }
    for t in tasks {
        t.await?;
    }
    let write_pairs = match writes {
        Some(t) => t.await?,
        None => 0,
    };
    let elapsed = start.elapsed().as_secs_f64();
    let info_after = json_ok(observer.get(base("/api/v1/system/info")), "reading system info").await?;

    // Acknowledged events must all be stored and queryable.
    let t = &run.totals;
    let acknowledged = t.acknowledged.load(Ordering::Relaxed);
    let expected = seeded as u64 + acknowledged;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut persisted = count(&admin, &args, &query).await?;
    while persisted != Some(expected) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
        persisted = count(&admin, &args, &query).await?;
    }

    // The server's session-lookup counters over the run.
    let (b, a) = (&info_before["sessionAuth"], &info_after["sessionAuth"]);
    let delta = |k: &str| a[k].as_u64().unwrap_or(0).saturating_sub(b[k].as_u64().unwrap_or(0));
    let lookups = delta("lookups");
    let mean = |k: &str| if lookups == 0 { Value::Null } else { json!(delta(k) as f64 / lookups as f64) };
    let load_ok = t.load.statuses.ok.load(Ordering::Relaxed);
    let summary = json!({
        "label": args.label,
        "auth": match args.auth { Auth::Session => "session", Auth::Key => "key" },
        "workload": match args.workload { Workload::Auth => "auth", Workload::Query => "query", Workload::Mixed => "mixed" },
        "durationS": elapsed,
        "args": {
            "concurrency": args.concurrency, "seedEvents": args.seed_events,
            "ingestConcurrency": if args.workload == Workload::Mixed { args.ingest_concurrency } else { 0 },
            "batch": args.batch, "writeIntervalMs": args.write_interval.as_millis() as u64,
            "probeIntervalMs": args.probe_interval.as_millis() as u64, "coldMs": args.cold.as_millis() as u64,
            "timeoutMs": args.timeout.as_millis() as u64,
        },
        "server": {
            "version": info_after["version"], "durability": info_after["durability"],
            "rssBytesAfter": info_after["memory"]["rssBytes"],
        },
        "load": {
            "requestsPerSecond": load_ok as f64 / elapsed,
            "all": t.load.summary(), "config": t.config.summary(), "search": t.search.summary(),
            "facets": t.facets.summary(), "dashboard": t.dashboard.summary(),
        },
        "probe": t.probe.summary(),
        "heartbeat": t.heartbeat.summary(),
        "ingest": {
            "requests": t.ingest.summary(), "acknowledgedEvents": acknowledged,
            "eventsPerSecond": acknowledged as f64 / elapsed,
            "seededEvents": seeded, "persistedEvents": persisted,
        },
        "writes": { "pairs": write_pairs, "requests": t.writes.summary() },
        "sessionLookups": {
            "cacheHits": delta("cacheHits"), "cached": a["cached"],
            "lookups": lookups, "perSecond": lookups as f64 / elapsed,
            "found": delta("found"), "failed": delta("failed"), "peakInFlight": a["peakInFlight"],
            "meanQueueMicros": mean("queueMicros"), "meanLockMicros": mean("lockMicros"),
            "meanQueryMicros": mean("queryMicros"),
            "maxQueueMicros": a["maxQueueMicros"], "maxLockMicros": a["maxLockMicros"],
            "maxQueryMicros": a["maxQueryMicros"],
        },
        "reconciled": persisted == Some(expected),
    });
    std::fs::write(args.out.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if persisted != Some(expected) {
        bail!("{expected} events acknowledged but {persisted:?} stored");
    }
    Ok(())
}
