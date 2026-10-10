//! Ingest admission (REL-02): `ingest.max_concurrent` bounds each request
//! from before its body is read until its write is acknowledged, excess
//! requests get a retryable 429 without their bodies being read, and every
//! way a request can end gives its slot back.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use telemetry::Signal;

use crate::common::*;

async fn limited(n: usize) -> TestServer {
    TestServer::start_with(|c| c.ingest.max_concurrent = n).await
}

/// Assert the documented "too many ingest requests" response.
async fn assert_rate_limited(r: reqwest::Response) {
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "1");
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "rate_limited");
}

async fn ingest_one(s: &TestServer, message: &str) -> reqwest::Response {
    s.post("/api/v1/events", json!({ "message": message })).await
}

// AC1: the limit covers body reading, decoding *and* submission. A request
// whose batch is parsed and queued but not yet acknowledged still holds its
// slot, on every ingest endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn admission_spans_decoding_and_submission() {
    let s = limited(1).await;
    let release = stall_writer(&s, Signal::Logs);

    let first = {
        let (client, url) = (s.client.clone(), s.u("/api/v1/events"));
        tokio::spawn(async move { client.post(url).json(&json!({ "message": "first" })).send().await.unwrap() })
    };
    // Parsed and queued: the request is now waiting for its acknowledgement.
    wait_until("first batch queued", || queue_depth(&s, Signal::Logs) == 1).await;
    assert_eq!(ingest_in_flight(&s), 1);

    assert_rate_limited(ingest_one(&s, "second").await).await;
    assert_rate_limited(s.otlp_json("/v1/logs", fixture("otlp-logs.json")).await).await;
    assert_rate_limited(s.otlp_json("/v1/traces", fixture("otlp-traces.json")).await).await;
    assert_eq!(queue_depth(&s, Signal::Logs), 1, "rejected requests queue nothing");

    drop(release);
    let r = first.await.unwrap();
    assert_eq!(r.status(), 200);
    wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
    assert_eq!(ingest_one(&s, "third").await.status(), 200);
    assert_eq!(s.messages("").await, ["first", "third"]);

    let info = s.get_ok("/api/v1/system/info").await;
    assert_eq!(info["ingest"]["maxConcurrent"], 1);
    assert_eq!(info["ingest"]["inFlight"], 0);
    assert_eq!(info["ingest"]["rejected"], 3);
    assert!(info["ingest"]["discarding"].as_u64().unwrap() <= 3);
    s.stop().await;
}

// AC2 / TC-02: with every slot held by slow uploads, further requests,
// compressed or not, are rejected promptly and their bodies are not read.
#[tokio::test(flavor = "multi_thread")]
async fn excess_requests_are_rejected_promptly_without_reading_bodies() {
    let s = limited(2).await;
    let body = ndjson_payload(200, 100);
    let compressed = gzip(body.as_bytes());

    let mut slow = Vec::new();
    for _ in 0..2 {
        let mut r =
            RawRequest::open(&s, "/api/v1/events", &[("Content-Type", "application/x-ndjson")], body.len()).await;
        r.send(&body.as_bytes()[..1024]).await;
        slow.push(r);
    }
    wait_until("slow uploads admitted", || ingest_in_flight(&s) == 2).await;
    let received = s.state().received_bytes.load(std::sync::atomic::Ordering::Relaxed);

    let started = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..32 {
        let (client, url) = (s.client.clone(), s.u("/api/v1/events"));
        let (plain, gz) = (body.clone(), compressed.clone());
        tasks.push(tokio::spawn(async move {
            let req = client.post(url).header("content-type", "application/x-ndjson");
            let req = if i % 2 == 0 { req.body(plain) } else { req.header("content-encoding", "gzip").body(gz) };
            let r = req.send().await.unwrap();
            (r.status().as_u16(), r.headers().get("retry-after").map(|v| v.to_str().unwrap().to_string()))
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), (429, Some("1".to_string())));
    }
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(5), "32 rejections took {elapsed:?}");
    assert_eq!(
        s.state().received_bytes.load(std::sync::atomic::Ordering::Relaxed),
        received,
        "no rejected body was read"
    );
    assert_eq!(s.state().ingest_rejected.load(std::sync::atomic::Ordering::Relaxed), 32);

    // The slow uploads still complete normally.
    for mut r in slow {
        r.send(&body.as_bytes()[1024..]).await;
        let (status, text) = r.response().await;
        assert_eq!(status, 200, "{text}");
    }
    wait_until("slots freed", || ingest_in_flight(&s) == 0).await;
    assert_eq!(s.query("").await["events"].as_array().unwrap().len(), 400);
    s.stop().await;
}

// AC3: every error a request can end with frees its slot. With one slot, a
// leak would turn the next request into a 429.
#[tokio::test(flavor = "multi_thread")]
async fn malformed_and_rejected_requests_release_their_slot() {
    let s = TestServer::start_with(|c| {
        c.ingest.max_concurrent = 1;
        c.ingest.max_request_size = server::config::ByteSize(64 << 10);
        c.ingest.queue_capacity = 10;
    })
    .await;
    let post = |path: &'static str, ct: &'static str, gz: bool, body: Vec<u8>| {
        let mut req = s.client.post(s.u(path)).header("content-type", ct);
        if gz {
            req = req.header("content-encoding", "gzip");
        }
        req.body(body).send()
    };

    let cases: Vec<(&str, reqwest::Response, u16)> = vec![
        ("invalid JSON", post("/api/v1/events", "application/json", false, b"{nope".to_vec()).await.unwrap(), 400),
        (
            "invalid NDJSON line",
            post("/api/v1/events", "application/x-ndjson", false, b"{}\n[1\n".to_vec()).await.unwrap(),
            400,
        ),
        (
            "invalid event",
            post("/api/v1/events", "application/json", false, b"[{\"message\":1},5]".to_vec()).await.unwrap(),
            400,
        ),
        (
            "over max_request_size",
            post("/api/v1/events", "application/x-ndjson", false, ndjson_payload(1000, 100).into_bytes())
                .await
                .unwrap(),
            413,
        ),
        (
            "gzip bomb",
            post("/api/v1/events", "application/x-ndjson", true, gzip(&vec![b' '; 1 << 20])).await.unwrap(),
            413,
        ),
        (
            "corrupt gzip",
            post("/api/v1/events", "application/json", true, b"not gzip at all".to_vec()).await.unwrap(),
            400,
        ),
        ("invalid protobuf", post("/v1/logs", "application/x-protobuf", false, vec![0xff; 64]).await.unwrap(), 400),
        (
            "invalid OTLP JSON",
            post("/v1/traces", "application/json", false, b"{\"resourceSpans\":7}".to_vec()).await.unwrap(),
            400,
        ),
        ("unsupported content type", post("/v1/metrics", "text/plain", false, b"x".to_vec()).await.unwrap(), 415),
        (
            "batch larger than the queue",
            post("/api/v1/events", "application/x-ndjson", false, ndjson_payload(11, 0).into_bytes()).await.unwrap(),
            413,
        ),
    ];
    for (name, r, status) in cases {
        assert_eq!(r.status().as_u16(), status, "{name}");
        wait_until(name, || ingest_in_flight(&s) == 0).await;
    }

    // Queue full at submission (storage's 429, not admission's): a batch
    // abandoned by its client fills the queue while the writer is parked.
    let release = stall_writer(&s, Signal::Logs);
    let body = ndjson_payload(10, 0);
    let mut abandoned =
        RawRequest::open(&s, "/api/v1/events", &[("Content-Type", "application/x-ndjson")], body.len()).await;
    abandoned.send(body.as_bytes()).await;
    wait_until("queue full", || queue_depth(&s, Signal::Logs) == 10).await;
    drop(abandoned);
    wait_until("abandoned slot freed", || ingest_in_flight(&s) == 0).await;
    let r = ingest_one(&s, "queue is full").await;
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["retry-after"], "1");
    wait_until("slot freed after queue full", || ingest_in_flight(&s) == 0).await;
    drop(release);
    wait_until("queue drained", || queue_depth(&s, Signal::Logs) == 0).await;

    assert_eq!(ingest_one(&s, "after errors").await.status(), 200);
    wait_until("slot freed", || ingest_in_flight(&s) == 0).await;
    assert_eq!(s.state().ingest_rejected.load(std::sync::atomic::Ordering::Relaxed), 0);
    s.stop().await;
}

// TC-03: a client that goes away frees its slot at every stage, and a later
// valid request is admitted.
#[tokio::test(flavor = "multi_thread")]
async fn cancelled_while_reading_body_releases_slot() {
    let s = limited(1).await;
    let body = ndjson_payload(100, 50);
    let mut r = RawRequest::open(&s, "/api/v1/events", &[("Content-Type", "application/x-ndjson")], body.len()).await;
    r.send(&body.as_bytes()[..body.len() / 2]).await;
    wait_until("upload admitted", || ingest_in_flight(&s) == 1).await;
    drop(r);
    wait_until("slot freed after disconnect", || ingest_in_flight(&s) == 0).await;
    assert_eq!(ingest_one(&s, "next").await.status(), 200);
    assert_eq!(s.messages("").await, ["next"]);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_while_decoding_releases_slot() {
    let s = TestServer::start_with(|c| {
        c.ingest.max_concurrent = 1;
        c.ingest.queue_capacity = 100_000;
    })
    .await;
    // Large enough that parsing is still running when the client leaves.
    let plain = ndjson_payload(50_000, 200);
    let body = gzip(plain.as_bytes());
    let mut r = RawRequest::open(
        &s,
        "/api/v1/events",
        &[("Content-Type", "application/x-ndjson"), ("Content-Encoding", "gzip")],
        body.len(),
    )
    .await;
    r.send(&body).await;
    // Leave once the whole body has been read, i.e. while it is parsed.
    let received = || s.state().received_bytes.load(std::sync::atomic::Ordering::Relaxed);
    wait_until("body read", || received() == plain.len() as u64).await;
    assert_eq!(ingest_in_flight(&s), 1);
    drop(r);
    wait_until("slot freed after disconnect", || ingest_in_flight(&s) == 0).await;
    assert_eq!(ingest_one(&s, "next").await.status(), 200);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_while_waiting_for_acknowledgement_releases_slot() {
    let s = limited(1).await;
    let release = stall_writer(&s, Signal::Logs);
    let body = br#"{"message":"abandoned"}"#;
    let mut r = RawRequest::open(&s, "/api/v1/events", &[("Content-Type", "application/json")], body.len()).await;
    r.send(body).await;
    wait_until("batch queued", || queue_depth(&s, Signal::Logs) == 1).await;
    assert_eq!(ingest_in_flight(&s), 1);
    drop(r);
    // Freed on disconnect, while the write is still waiting on the writer.
    wait_until("slot freed after disconnect", || ingest_in_flight(&s) == 0).await;
    assert_eq!(queue_depth(&s, Signal::Logs), 1);

    let next = {
        let (client, url) = (s.client.clone(), s.u("/api/v1/events"));
        tokio::spawn(async move { client.post(url).json(&json!({ "message": "next" })).send().await.unwrap() })
    };
    wait_until("next batch queued", || queue_depth(&s, Signal::Logs) == 2).await;
    drop(release);
    assert_eq!(next.await.unwrap().status(), 200);
    // The abandoned batch was already queued, so it is still written.
    assert_eq!(s.messages("").await, ["abandoned", "next"]);
    s.stop().await;
}
