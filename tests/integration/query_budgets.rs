//! Query budgets and cancellation (REL-12) through the HTTP API: limit
//! errors, client disconnects, query slots under concurrent ingest, and
//! complete answers afterwards.
//!
//! Slow queries are made with the engine's checkpoint hook rather than with
//! large datasets, so every test is deterministic: [`hold_queries`] parks
//! each query at its first checkpoint until it is cancelled or released.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use query::Checkpoint;
use serde_json::{Value, json};
use telemetry::Signal;

use crate::common::*;

/// Queries parked by [`hold_queries`].
struct Hold {
    held: Arc<AtomicUsize>,
    max_held: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
}

impl Hold {
    fn held(&self) -> usize {
        self.held.load(Ordering::SeqCst)
    }

    /// Let parked and future queries run.
    fn release(&self) {
        self.release.store(true, Ordering::SeqCst);
    }
}

/// Park every query at its start checkpoint until its request is cancelled
/// or [`Hold::release`] is called (or 30 s pass, so a broken test fails
/// instead of hanging).
fn hold_queries(s: &TestServer) -> Hold {
    let hold = Hold {
        held: Default::default(),
        max_held: Default::default(),
        cancelled: Default::default(),
        release: Default::default(),
    };
    let (held, max_held, cancelled, release) =
        (hold.held.clone(), hold.max_held.clone(), hold.cancelled.clone(), hold.release.clone());
    s.state().engine.set_checkpoint_hook(Some(Arc::new(move |at, token| {
        if at != Checkpoint::Start || release.load(Ordering::SeqCst) {
            return;
        }
        let now = held.fetch_add(1, Ordering::SeqCst) + 1;
        max_held.fetch_max(now, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !token.is_cancelled() && !release.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        if token.is_cancelled() {
            cancelled.fetch_add(1, Ordering::SeqCst);
        }
        held.fetch_sub(1, Ordering::SeqCst);
    })));
    hold
}

/// Queries holding a slot right now.
fn queries_in_flight(s: &TestServer) -> usize {
    s.state().config.query.max_concurrent - s.state().query_permits.available_permits()
}

async fn error_of(r: reqwest::Response) -> (u16, Value) {
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap();
    (status, v["error"].clone())
}

async fn count(s: &TestServer, q: &str) -> u64 {
    s.post_ok("/api/v1/query/logs/count", json!({ "query": q })).await["count"].as_u64().unwrap()
}

fn spawn_query(s: &TestServer, body: Value) -> tokio::task::JoinHandle<(u16, Value)> {
    let (client, url) = (s.client.clone(), s.u("/api/v1/query/logs"));
    tokio::spawn(async move {
        let r = client.post(url).json(&body).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap())
    })
}

// AC2/AC4: a client that disconnects mid-query cancels it. The query stops
// at its next checkpoint, gives its slot back, and the next query gets a
// complete answer.
#[tokio::test(flavor = "multi_thread")]
async fn client_disconnect_cancels_the_query_and_frees_its_slot() {
    let s = TestServer::start_with(|c| {
        c.query.max_concurrent = 1;
        c.query.queue_timeout = Duration::from_millis(100);
    })
    .await;
    s.ingest(json!([{ "message": "a" }, { "message": "b" }, { "message": "c" }])).await;
    let hold = hold_queries(&s);

    let body = json!({ "query": "", "limit": 10 }).to_string();
    let mut raw = RawRequest::open(&s, "/api/v1/query/logs", &[("Content-Type", "application/json")], body.len()).await;
    raw.send(body.as_bytes()).await;
    wait_until("query parked", || hold.held() == 1).await;
    assert_eq!(queries_in_flight(&s), 1);

    // Its slot is taken: the next query waits `queue_timeout`, then 503.
    let r = s.post("/api/v1/query/logs", json!({ "query": "" })).await;
    assert_eq!(r.headers()["retry-after"], "1");
    let (status, err) = error_of(r).await;
    assert_eq!((status, err["code"].as_str()), (503, Some("unavailable")), "{err}");
    assert_eq!(err["message"], "all 1 query slots are busy; try again shortly");

    drop(raw);
    wait_until("cancelled query gives its slot back", || queries_in_flight(&s) == 0).await;
    assert_eq!(hold.cancelled.load(Ordering::SeqCst), 1, "stopped by cancellation, not by the hold timing out");

    hold.release();
    assert_eq!(s.messages("").await, ["a", "b", "c"]);
    let info = s.get_ok("/api/v1/system/info").await;
    assert_eq!(info["query"]["maxConcurrent"], 1);
    assert_eq!(info["query"]["inFlight"], 0);
    assert_eq!(info["query"]["cancelled"], 1);
    assert_eq!(info["query"]["rejected"], 1);
    s.stop().await;
}

// AC3: with every query slot held by slow queries, further queries are
// refused promptly instead of queueing blocking work, and ingestion is
// unaffected. Released queries then complete in full.
#[tokio::test(flavor = "multi_thread")]
async fn query_slots_bound_blocking_work_while_ingest_continues() {
    let s = TestServer::start_with(|c| {
        c.query.max_concurrent = 2;
        c.query.queue_timeout = Duration::from_millis(100);
        // Ingest has its own admission limit (REL-02); room for every
        // request here, so a refusal could only come from queries.
        c.ingest.max_concurrent = 20;
    })
    .await;
    let seed: Vec<Value> = (0..50).map(|i| json!({ "message": format!("seed {i}") })).collect();
    s.ingest(Value::Array(seed)).await;
    let hold = hold_queries(&s);
    let slow: Vec<_> = (0..2).map(|_| spawn_query(&s, json!({ "query": "", "limit": 1000 }))).collect();
    wait_until("both slots held", || hold.held() == 2).await;

    let started = Instant::now();
    let refused: Vec<_> = (0..8).map(|_| spawn_query(&s, json!({ "query": "" }))).collect();
    let ingest: Vec<_> = (0..20)
        .map(|i| {
            let (client, url) = (s.client.clone(), s.u("/api/v1/events"));
            tokio::spawn(async move {
                client.post(url).json(&json!({ "message": format!("during {i}") })).send().await.unwrap().status()
            })
        })
        .collect();
    for r in ingest {
        assert_eq!(r.await.unwrap(), 200, "ingest is not blocked by queries");
    }
    for r in refused {
        let (status, v) = r.await.unwrap();
        assert_eq!((status, v["error"]["code"].as_str()), (503, Some("unavailable")), "{v}");
    }
    assert!(started.elapsed() < Duration::from_secs(5), "refused promptly: {:?}", started.elapsed());
    assert_eq!(hold.max_held.load(Ordering::SeqCst), 2, "never more queries executing than slots");
    assert_eq!(queries_in_flight(&s), 2);

    hold.release();
    for q in slow {
        let (status, v) = q.await.unwrap();
        assert_eq!(status, 200, "{v}");
        // Each answer is complete for the data it started on.
        assert_eq!(v["events"].as_array().unwrap().len(), 50);
        assert!(v["continuationToken"].is_null());
    }
    assert_eq!(count(&s, "").await, 70);
    let info = s.get_ok("/api/v1/system/info").await;
    assert_eq!(info["query"]["rejected"], 8);
    assert_eq!(info["query"]["inFlight"], 0);
    s.stop().await;
}

// AC1/TC-03: a query over `query.timeout` fails with 504 and returns no
// rows; the next small query on the same server is complete.
#[tokio::test(flavor = "multi_thread")]
async fn timeout_is_an_explicit_error_and_the_server_stays_usable() {
    let s = TestServer::start_with(|c| c.query.timeout = Duration::from_millis(50)).await;
    s.ingest(json!([{ "message": "x", "level": "Error" }, { "message": "y" }])).await;
    let slow = Arc::new(AtomicBool::new(true));
    let armed = slow.clone();
    s.state().engine.set_checkpoint_hook(Some(Arc::new(move |at, _| {
        if at == Checkpoint::Start && armed.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
    })));

    for path in ["/api/v1/query/logs", "/api/v1/query/logs/count", "/api/v1/query/logs/facets"] {
        let (status, err) = error_of(s.post(path, json!({ "query": "" })).await).await;
        assert_eq!((status, err["code"].as_str()), (504, Some("query_timeout")), "{path}: {err}");
        assert_eq!(err["limitMs"], 50);
        assert_eq!(err["message"], "query exceeded its time limit of 50 ms");
    }

    slow.store(false, Ordering::SeqCst);
    assert_eq!(s.messages("").await, ["x", "y"]);
    assert_eq!(count(&s, "level = Error").await, 1);
    let info = s.get_ok("/api/v1/system/info").await;
    assert_eq!(info["query"]["timedOut"], 3);
    assert_eq!(info["query"]["inFlight"], 0);
    s.stop().await;
}

// AC1/TC-02: scan and result budgets fail with explicit errors that name
// the limit; a request inside the budgets still works afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn scan_and_result_budgets_are_explicit_errors() {
    let s = TestServer::start_with(|c| {
        c.query.max_scan_bytes = server::config::ByteSize(1);
        c.query.max_trace_spans = 2;
    })
    .await;
    s.ingest(json!([{ "message": "sealed one" }, { "message": "sealed two" }])).await;
    s.state().storage.rotate_blocking(Signal::Logs).unwrap();
    // Three spans of one trace, left unsealed (no bytes to read).
    assert_eq!(s.otlp_json("/v1/traces", fixture("otlp-traces.json")).await.status(), 200);

    let (status, err) =
        error_of(s.post("/api/v1/query/logs", json!({ "query": "message contains \"one\"" })).await).await;
    assert_eq!((status, err["code"].as_str()), (422, Some("query_too_expensive")), "{err}");
    assert_eq!(err["limitBytes"], 1);

    let trace = "/api/v1/traces/4bf92f3577b34da6a3ce929d0e0e4736";
    let (status, err) = error_of(s.client.get(s.u(trace)).send().await.unwrap()).await;
    assert_eq!((status, err["code"].as_str()), (422, Some("query_result_limit")), "{err}");
    assert_eq!(err["limit"], 2);
    assert!(err["message"].as_str().unwrap().contains("2 spans in one trace"), "{err}");

    let metric = json!({ "name": "cpu", "from": "-3h", "stepMs": 1000 });
    let (status, err) = error_of(s.post("/api/v1/query/metrics", metric).await).await;
    assert_eq!((status, err["code"].as_str()), (422, Some("query_result_limit")), "{err}");
    assert_eq!(err["limit"], 10_000);

    // Page size over `query.max_results` is refused before execution.
    let (status, err) = error_of(s.post("/api/v1/query/logs", json!({ "query": "", "limit": 1001 })).await).await;
    assert_eq!((status, err["message"].as_str()), (400, Some("limit must be between 1 and 1000")), "{err}");

    // Within budget: summaries answer an unfiltered count without reading,
    // and trace search reports that its summary is complete.
    assert_eq!(count(&s, "").await, 2);
    let traces = s.get_ok("/api/v1/traces").await;
    assert_eq!(traces["traces"][0]["spanCount"], 3);
    assert_eq!(traces["traces"][0]["truncated"], false);
    let info = s.get_ok("/api/v1/system/info").await;
    assert_eq!(info["query"]["tooExpensive"], 1);
    assert_eq!(info["query"]["resultLimited"], 2);
    s.stop().await;
}
