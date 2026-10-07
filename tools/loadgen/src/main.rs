//! Load generator for Vyrtel.
//!
//! ```text
//! cargo run --release -p loadgen -- --target http://localhost:8080 --rate 10000 --duration 60s
//! ```
//!
//! Sends NDJSON batches of realistic structured logs (and optionally OTLP
//! spans) at a target rate, then reports achieved throughput, latency and
//! backpressure (429) responses.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use clap::Parser;
use rand::Rng;
use rand::rngs::StdRng;
use rand::{SeedableRng, seq::SliceRandom};
use serde_json::json;

#[derive(Parser, Clone)]
#[command(name = "loadgen", about = "Generate realistic telemetry load against Vyrtel")]
struct Args {
    /// Vyrtel base URL.
    #[arg(long, default_value = "http://localhost:8080")]
    target: String,
    /// Target events per second (total across workers).
    #[arg(long, default_value_t = 1000)]
    rate: u64,
    /// How long to run, e.g. 30s, 5m.
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    duration: Duration,
    /// Number of distinct services.
    #[arg(long, default_value_t = 8)]
    services: usize,
    /// Fraction of events that are errors (0.0–1.0).
    #[arg(long, default_value_t = 0.02)]
    error_rate: f64,
    /// Distinct values for high-cardinality properties (customerId, ...).
    #[arg(long, default_value_t = 10_000)]
    cardinality: u64,
    /// Events per request.
    #[arg(long, default_value_t = 500)]
    batch: usize,
    /// Concurrent senders.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
    /// Also send one OTLP trace (3 spans) per this many log events (0 = off).
    #[arg(long, default_value_t = 0)]
    traces_every: u64,
    /// API key, if the server requires one.
    #[arg(long, env = "VYRTEL_API_KEY")]
    api_key: Option<String>,
    /// RNG seed for reproducible data.
    #[arg(long, default_value_t = 42)]
    seed: u64,
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

const SERVICES: &[&str] = &[
    "payments",
    "orders",
    "auth",
    "inventory",
    "search",
    "shipping",
    "notifications",
    "gateway",
    "billing",
    "catalog",
    "recommendations",
    "users",
];
const ROUTES: &[&str] = &["/api/orders", "/api/pay", "/api/login", "/api/search", "/api/cart", "/health"];
const PROVIDERS: &[&str] = &["stripe", "adyen", "paypal"];
const REGIONS: &[&str] = &["eu-west-1", "us-east-1", "ap-southeast-2"];
const ERRORS: &[(&str, &str)] = &[
    ("TimeoutException", "Payment provider timed out"),
    ("DbException", "deadlock detected while updating order"),
    ("HttpRequestException", "upstream returned 503"),
    ("InvalidOperationException", "inventory reservation conflict"),
];

struct Gen {
    rng: StdRng,
    args: Args,
}

fn now_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}

impl Gen {
    fn trace_id(&mut self) -> String {
        format!("{:032x}", self.rng.r#gen::<u128>())
    }

    fn log(&mut self) -> serde_json::Value {
        let svc = SERVICES[self.rng.gen_range(0..self.args.services.min(SERVICES.len()).max(1))];
        let customer = self.rng.gen_range(0..self.args.cardinality.max(1));
        let route = *ROUTES.choose(&mut self.rng).unwrap();
        let duration = (self.rng.r#gen::<f64>().powi(3) * 2000.0).round();
        let is_error = self.rng.r#gen::<f64>() < self.args.error_rate;
        let mut ev = json!({
            "service": svc,
            "environment": if self.rng.gen_bool(0.9) { "production" } else { "staging" },
            "traceId": self.trace_id(),
            "properties": {
                "customerId": customer,
                "requestId": format!("req-{:016x}", self.rng.r#gen::<u64>()),
                "durationMs": duration,
                "region": REGIONS.choose(&mut self.rng).unwrap(),
                "http": { "method": if route == "/api/search" { "GET" } else { "POST" }, "route": route,
                          "statusCode": if is_error { 500 } else if self.rng.gen_bool(0.05) { 404 } else { 200 } },
            },
        });
        if is_error {
            let (kind, msg) = *ERRORS.choose(&mut self.rng).unwrap();
            ev["level"] = json!("Error");
            ev["message"] = json!(format!("{msg} (customer {customer})"));
            ev["messageTemplate"] = json!(format!("{msg} (customer {{customerId}})"));
            ev["exception"] = json!({
                "type": kind,
                "message": msg,
                "stackTrace": format!("{kind}: {msg}\n   at {svc}.Handler.Handle() in Handler.cs:line {}\n   at {svc}.Pipeline.Run()", self.rng.gen_range(10..400)),
            });
            if svc == "payments" {
                ev["properties"]["paymentProvider"] = json!(PROVIDERS.choose(&mut self.rng).unwrap());
            }
        } else {
            let level = if duration > 1500.0 {
                "Warning"
            } else if self.rng.gen_bool(0.1) {
                "Debug"
            } else {
                "Information"
            };
            ev["level"] = json!(level);
            ev["message"] = json!(format!(
                "{} {route} responded in {duration} ms",
                if route == "/api/search" { "GET" } else { "POST" }
            ));
            ev["messageTemplate"] = json!("{Method} {Route} responded in {DurationMs} ms");
        }
        ev
    }

    fn trace(&mut self) -> serde_json::Value {
        let trace = self.trace_id();
        let now = now_nanos();
        let total = self.rng.gen_range(5_000_000u128..900_000_000);
        let start = now - total;
        let span = |id: &str, parent: Option<&str>, name: &str, s: u128, e: u128, err: bool| {
            let mut v = json!({
                "traceId": trace, "spanId": id, "name": name, "kind": 2,
                "startTimeUnixNano": s.to_string(), "endTimeUnixNano": e.to_string(),
                "status": { "code": if err { 2 } else { 1 } },
            });
            if let Some(p) = parent {
                v["parentSpanId"] = json!(p);
            }
            v
        };
        let err = self.rng.r#gen::<f64>() < self.args.error_rate;
        let a = format!("{:016x}", self.rng.r#gen::<u64>());
        let b = format!("{:016x}", self.rng.r#gen::<u64>());
        let c = format!("{:016x}", self.rng.r#gen::<u64>());
        let svc = SERVICES[self.rng.gen_range(0..self.args.services.min(SERVICES.len()).max(1))];
        json!({ "resourceSpans": [
            { "resource": { "attributes": [ { "key": "service.name", "value": { "stringValue": "gateway" } } ] },
              "scopeSpans": [ { "spans": [ span(&a, None, "POST /api", start, now, err) ] } ] },
            { "resource": { "attributes": [ { "key": "service.name", "value": { "stringValue": svc } } ] },
              "scopeSpans": [ { "spans": [
                  span(&b, Some(&a), "handle request", start + total / 10, now - total / 10, err),
                  span(&c, Some(&b), "SELECT", start + total / 5, start + total / 2, false),
              ] } ] }
        ] })
    }
}

#[derive(Default)]
struct Stats {
    sent: AtomicU64,
    requests: AtomicU64,
    rejected: AtomicU64,
    failed: AtomicU64,
    latency_us: AtomicU64,
    max_latency_us: AtomicU64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.rate == 0 || args.batch == 0 || args.concurrency == 0 {
        bail!("--rate, --batch and --concurrency must be positive");
    }
    let client = reqwest::Client::builder().pool_max_idle_per_host(args.concurrency).build()?;
    let health = client
        .get(format!("{}/health", args.target))
        .send()
        .await
        .with_context(|| format!("cannot reach {}", args.target))?;
    if !health.status().is_success() {
        bail!("{} /health returned {}", args.target, health.status());
    }
    println!(
        "loadgen: {} events/s for {:?} → {} (batch {}, {} workers, {} services, error rate {})",
        args.rate, args.duration, args.target, args.batch, args.concurrency, args.services, args.error_rate
    );

    let stats = Arc::new(Stats::default());
    let started = Instant::now();
    let deadline = started + args.duration;
    // Each worker sends batches on a fixed schedule so the total matches
    // --rate; if the server is slower than that, we fall behind (and report).
    let per_worker_rate = args.rate as f64 / args.concurrency as f64;
    let interval = Duration::from_secs_f64(args.batch as f64 / per_worker_rate);
    let mut workers = Vec::new();
    for w in 0..args.concurrency {
        let args = args.clone();
        let client = client.clone();
        let stats = stats.clone();
        workers.push(tokio::spawn(async move {
            let mut g = Gen { rng: StdRng::seed_from_u64(args.seed + w as u64), args: args.clone() };
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut since_trace = 0u64;
            while Instant::now() < deadline {
                tick.tick().await;
                let mut body = String::with_capacity(args.batch * 600);
                for _ in 0..args.batch {
                    body.push_str(&g.log().to_string());
                    body.push('\n');
                }
                let mut req = client
                    .post(format!("{}/api/v1/events", args.target))
                    .header("content-type", "application/x-ndjson")
                    .body(body);
                if let Some(k) = &args.api_key {
                    req = req.bearer_auth(k);
                }
                let t = Instant::now();
                let res = req.send().await;
                let us = t.elapsed().as_micros() as u64;
                stats.requests.fetch_add(1, Ordering::Relaxed);
                stats.latency_us.fetch_add(us, Ordering::Relaxed);
                stats.max_latency_us.fetch_max(us, Ordering::Relaxed);
                match res {
                    Ok(r) if r.status().is_success() => {
                        stats.sent.fetch_add(args.batch as u64, Ordering::Relaxed);
                    }
                    Ok(r) if r.status().as_u16() == 429 || r.status().as_u16() == 503 => {
                        stats.rejected.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {
                        stats.failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if args.traces_every > 0 {
                    since_trace += args.batch as u64;
                    while since_trace >= args.traces_every {
                        since_trace -= args.traces_every;
                        let mut req = client.post(format!("{}/v1/traces", args.target)).json(&g.trace());
                        if let Some(k) = &args.api_key {
                            req = req.bearer_auth(k);
                        }
                        let _ = req.send().await;
                    }
                }
            }
        }));
    }

    let reporter = {
        let stats = stats.clone();
        tokio::spawn(async move {
            let mut last = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let sent = stats.sent.load(Ordering::Relaxed);
                println!(
                    "  {:>5.0}s  {:>9} events  {:>8.0} ev/s  rejected {}  failed {}",
                    started.elapsed().as_secs_f64(),
                    sent,
                    (sent - last) as f64 / 5.0,
                    stats.rejected.load(Ordering::Relaxed),
                    stats.failed.load(Ordering::Relaxed)
                );
                last = sent;
            }
        })
    };
    for w in workers {
        w.await?;
    }
    reporter.abort();

    let secs = started.elapsed().as_secs_f64();
    let sent = stats.sent.load(Ordering::Relaxed);
    let reqs = stats.requests.load(Ordering::Relaxed).max(1);
    println!("\nresult:");
    println!("  events accepted    {sent}");
    println!("  throughput         {:.0} events/s (target {})", sent as f64 / secs, args.rate);
    println!("  requests           {reqs}");
    println!("  mean latency       {:.1} ms", stats.latency_us.load(Ordering::Relaxed) as f64 / reqs as f64 / 1000.0);
    println!("  max latency        {:.1} ms", stats.max_latency_us.load(Ordering::Relaxed) as f64 / 1000.0);
    println!("  backpressure (429/503) {}", stats.rejected.load(Ordering::Relaxed));
    println!("  failures           {}", stats.failed.load(Ordering::Relaxed));
    Ok(())
}
