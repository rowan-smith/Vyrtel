//! REL-05: graceful shutdown drains accepted work within
//! `server.shutdown_timeout`, refuses new work, reports what it could not
//! finish, and ends live-tail streams so clients reconnect after a restart.
//!
//! In-process tests drive the stop request directly (what the signal
//! handlers in `main.rs` call); the `process` tests send real SIGTERMs to the
//! `vyrtel` binary (Unix only: Windows console control events cannot be sent
//! to a child that has no console of its own).

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use server::app::{IncompleteShutdown, ShutdownPhase, StopCause, Unfinished};
use telemetry::Signal;

use crate::common::*;

fn batch(tag: &str, n: usize) -> Value {
    Value::Array((0..n).map(|i| json!({"message": format!("{tag}-{i}"), "service": "drain"})).collect())
}

/// Post `body` to the native endpoint in the background.
fn post_in_background(s: &TestServer, body: Value) -> tokio::task::JoinHandle<reqwest::Result<(u16, Value)>> {
    let (client, url) = (s.client.clone(), s.u("/api/v1/events"));
    tokio::spawn(async move {
        let r = client.post(url).json(&body).send().await?;
        let status = r.status().as_u16();
        Ok((status, r.json().await.unwrap_or(Value::Null)))
    })
}

fn incomplete(r: anyhow::Result<()>) -> IncompleteShutdown {
    r.expect_err("shutdown must report unfinished work").downcast::<IncompleteShutdown>().unwrap()
}

// AC1 + AC2 (TC-02, full queue): a stop request with the queue full waits
// for every admitted request to be acknowledged, turns new work away, and
// all acknowledged events are back after a restart.
#[tokio::test(flavor = "multi_thread")]
async fn stop_drains_a_full_queue_refuses_new_work_and_keeps_acknowledged_events() {
    let mut s = TestServer::start_with(|c| {
        c.ingest.max_concurrent = 8;
        c.ingest.queue_capacity = 40;
        c.server.shutdown_timeout = Duration::from_secs(20);
    })
    .await;
    s.ingest(batch("before", 3)).await;
    s.rotate_all();
    let release = stall_writer(&s, Signal::Logs);
    let accepted: Vec<_> = (0..4).map(|r| post_in_background(&s, batch(&format!("queued{r}"), 10))).collect();
    wait_until("queue full", || queue_depth(&s, Signal::Logs) == 40).await;
    assert_eq!(ingest_in_flight(&s), 4);
    // Unchanged behaviour: a full queue turns the next batch away.
    let r = s.client.post(s.u("/api/v1/events")).json(&batch("overflow", 1)).send().await.unwrap();
    assert_eq!(r.status(), 429);

    s.begin_stop();
    wait_until("draining", || s.state().draining()).await;
    let began = Instant::now();
    // New work is refused: the listener is closed (or, on a connection
    // that is still open, the request gets 503).
    let late = authorized_client("authorization", &format!("Bearer {}", s.api_key))
        .post(s.u("/api/v1/events"))
        .json(&batch("late", 1))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    if let Ok(r) = late {
        assert_eq!(r.status(), 503);
    }
    // The drain waits for the accepted requests.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(ingest_in_flight(&s), 4, "accepted requests are still being waited for");
    assert!(accepted.iter().all(|h| !h.is_finished()));

    drop(release);
    for h in accepted {
        let (status, body) = h.await.unwrap().expect("accepted request gets its response");
        assert_eq!((status, &body["accepted"]), (200, &json!(10)), "{body}");
    }
    let (r, config, dir) = s.stopped().await;
    r.expect("drained within the deadline");
    let took = began.elapsed();
    assert!(took < Duration::from_secs(20), "drain took {took:?}");

    let s = TestServer::launch(config, dir).await;
    let mut expected: Vec<String> = (0..3).map(|i| format!("before-{i}")).collect();
    for r in 0..4 {
        expected.extend((0..10).map(|i| format!("queued{r}-{i}")));
    }
    expected.sort();
    assert_eq!(s.messages("service = drain").await, expected, "acknowledged once each; nothing refused");
    s.stop().await;
}

// TC-02 (empty queue): with nothing in flight, stopping takes no longer
// than flushing the WAL, and everything acknowledged is back.
#[tokio::test(flavor = "multi_thread")]
async fn stop_with_an_empty_queue_is_prompt() {
    let s = TestServer::start().await;
    let r = s.ingest_ndjson(fixture("logs.ndjson")).await;
    assert_eq!(r.status(), 200);
    let accepted = r.json::<Value>().await.unwrap()["accepted"].as_u64().unwrap();
    let began = Instant::now();
    let (config, dir) = s.stop().await;
    let took = began.elapsed();
    assert!(took < Duration::from_secs(2), "idle stop took {took:?}");
    let s = TestServer::launch(config, dir).await;
    assert_eq!(s.query("").await["events"].as_array().unwrap().len() as u64, accepted);
    s.stop().await;
}

// AC3 + TC-03: when the deadline passes with a request still waiting for
// its acknowledgement, shutdown stops waiting on time and names the work.
#[tokio::test(flavor = "multi_thread")]
async fn deadline_expiry_reports_unfinished_requests() {
    let mut s = TestServer::start_with(|c| c.server.shutdown_timeout = Duration::from_millis(500)).await;
    let release = stall_writer(&s, Signal::Logs);
    let pending = post_in_background(&s, batch("stuck", 5));
    wait_until("queued", || queue_depth(&s, Signal::Logs) == 5).await;

    s.begin_stop();
    let began = Instant::now();
    let (r, _, _dir) = s.stopped().await;
    let took = began.elapsed();
    let e = incomplete(r);
    assert!((Duration::from_millis(450)..Duration::from_secs(3)).contains(&took), "gave up after {took:?}");
    assert_eq!(e.cause, StopCause::Deadline(Duration::from_millis(500)));
    assert_eq!(e.phase, ShutdownPhase::Requests);
    assert_eq!(e.unfinished, Unfinished { ingest_requests: 1, queued: vec![(Signal::Logs, 5)], live_streams: 0 });
    let text = e.to_string();
    for part in [
        "server.shutdown_timeout (500ms) passed while waiting for in-flight requests",
        "1 ingest request not acknowledged",
        "5 queued events not written (logs 5)",
        "Acknowledged events are already in the WAL",
    ] {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    assert!(!pending.is_finished(), "the request was never acknowledged");
    drop(release);
}

// TC-03: a second stop request ends the wait at once, long before the
// deadline.
#[tokio::test(flavor = "multi_thread")]
async fn second_stop_request_stops_waiting() {
    let mut s = TestServer::start_with(|c| c.server.shutdown_timeout = Duration::from_secs(60)).await;
    let release = stall_writer(&s, Signal::Logs);
    let _pending = post_in_background(&s, batch("stuck", 2));
    wait_until("queued", || queue_depth(&s, Signal::Logs) == 2).await;

    s.begin_stop();
    wait_until("draining", || s.state().draining()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let began = Instant::now();
    s.force_stop();
    let (r, _, _dir) = s.stopped().await;
    let took = began.elapsed();
    assert!(took < Duration::from_secs(2), "second stop request took {took:?}");
    let e = incomplete(r);
    assert_eq!((&e.cause, e.phase), (&StopCause::Forced, ShutdownPhase::Requests));
    assert_eq!(e.unfinished.ingest_requests, 1);
    assert!(e.to_string().starts_with("shutdown incomplete: a second stop signal arrived"), "{e}");
    drop(release);
}

// TC-03: the deadline also bounds the final storage flush, not only HTTP.
#[tokio::test(flavor = "multi_thread")]
async fn deadline_bounds_a_stuck_storage_flush() {
    let s = TestServer::start_with(|c| c.server.shutdown_timeout = Duration::from_millis(500)).await;
    let release = stall_writer(&s, Signal::Traces);
    // Queued without an HTTP request, so the request phase drains at once.
    let events = ingest::native::parse(
        br#"{"message":"queued"}"#,
        ingest::native::Format::Json,
        telemetry::Timestamp::now(),
        ingest::Limits::UNBOUNDED,
    )
    .unwrap();
    s.state().storage.submit(Signal::Traces, events, Box::new(|_| {})).unwrap();

    let began = Instant::now();
    let (r, _, _dir) = s.stopped().await;
    let took = began.elapsed();
    let e = incomplete(r);
    assert!(took < Duration::from_secs(3), "gave up after {took:?}");
    assert_eq!(e.phase, ShutdownPhase::Storage);
    assert_eq!(e.unfinished, Unfinished { ingest_requests: 0, queued: vec![(Signal::Traces, 1)], live_streams: 0 });
    assert!(e.to_string().contains("while storage was writing queued batches and flushing the WAL"), "{e}");
    drop(release);
}

/// Park the maintenance thread of `signal` in the completion callback of a
/// seal of its (non-empty) active buffer, until the sender is dropped.
fn park_maintenance(s: &TestServer, signal: Signal) -> std::sync::mpsc::Sender<()> {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    s.state().storage.stream(signal).rotate(Box::new(move |_| {
        let _ = entered_tx.send(());
        let _ = release_rx.recv();
    }));
    entered_rx.recv_timeout(Duration::from_secs(10)).expect("maintenance thread parked");
    release_tx
}

/// Wait until shutdown has flushed the WALs and moved on to the later
/// phases: storage refuses writes from the moment the flush starts.
async fn wait_for_flush_to_start(s: &TestServer) {
    let storage = s.state().storage.clone();
    wait_until("storage refuses writes", || {
        let events = ingest::native::parse(
            br#"{"message":"probe"}"#,
            ingest::native::Format::Json,
            telemetry::Timestamp::now(),
            ingest::Limits::UNBOUNDED,
        )
        .unwrap();
        storage.submit(Signal::Metrics, events, Box::new(|_| {})).is_err()
    })
    .await;
}

// TC-03: a seal still running at the deadline holds shutdown no longer than
// the deadline and is reported as the phase that did not finish, saying
// every accepted request was flushed. Its input is in the WAL, so the next
// start has every event.
#[tokio::test(flavor = "multi_thread")]
async fn a_running_seal_is_bounded_by_the_deadline_and_reported() {
    let s = TestServer::start_with(|c| c.server.shutdown_timeout = Duration::from_millis(500)).await;
    s.ingest(batch("sealed", 3)).await;
    let release = park_maintenance(&s, Signal::Logs);

    let began = Instant::now();
    let (r, config, dir) = s.stopped().await;
    let took = began.elapsed();
    assert!(took < Duration::from_secs(3), "stopped after {took:?}");
    let e = incomplete(r);
    assert_eq!((&e.cause, e.phase), (&StopCause::Deadline(Duration::from_millis(500)), ShutdownPhase::Maintenance));
    assert_eq!(e.unfinished, Unfinished { ingest_requests: 0, queued: vec![], live_streams: 0 });
    let text = e.to_string();
    assert!(
        text.contains("while a seal or compaction was finishing, after every accepted request was written"),
        "{text}"
    );
    drop(release);
    let s = TestServer::launch(config, dir).await;
    assert_eq!(s.messages("service = drain").await, ["sealed-0", "sealed-1", "sealed-2"]);
    s.stop().await;
}

// TC-03: a second stop request is honoured in the last phase too, while a
// background seal is deliberately blocked: shutdown stops at once and says
// it was forced, instead of returning success.
#[tokio::test(flavor = "multi_thread")]
async fn second_stop_request_ends_a_blocked_seal() {
    let mut s = TestServer::start_with(|c| c.server.shutdown_timeout = Duration::from_secs(60)).await;
    s.ingest(batch("sealed", 3)).await;
    let release = park_maintenance(&s, Signal::Logs);

    s.begin_stop();
    wait_for_flush_to_start(&s).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let began = Instant::now();
    s.force_stop();
    let (r, config, dir) = s.stopped().await;
    let took = began.elapsed();
    assert!(took < Duration::from_secs(2), "second stop request took {took:?}");
    let e = incomplete(r);
    assert_eq!((&e.cause, e.phase), (&StopCause::Forced, ShutdownPhase::Maintenance));
    assert!(e.to_string().starts_with("shutdown incomplete: a second stop signal arrived while a seal"), "{e}");
    drop(release);
    let s = TestServer::launch(config, dir).await;
    assert_eq!(s.messages("service = drain").await, ["sealed-0", "sealed-1", "sealed-2"]);
    s.stop().await;
}

// AC4 + TC-02 (slow consumer): open live-tail streams, one of which has not
// read anything for a while, end cleanly with a `shutdown` event carrying a
// reconnection delay, and a client that reconnects reaches the restarted
// server on the same address.
#[tokio::test(flavor = "multi_thread")]
async fn live_streams_end_cleanly_and_reconnect_after_restart() {
    let s = TestServer::start().await;
    let addr = s.url.trim_start_matches("http://").to_string();
    let mut slow = Sse::open(&s, "").await;
    let mut errors = Sse::open(&s, "level = Error").await;
    let events: Vec<Value> = (0..300).map(|i| json!({"message": format!("live-{i}"), "level": "Error"})).collect();
    for chunk in events.chunks(100) {
        s.ingest(Value::Array(chunk.to_vec())).await;
    }
    let (k, _) = errors.next().await;
    assert_eq!(k, "event");

    let began = Instant::now();
    let (config, dir) = s.stop().await;
    let took = began.elapsed();
    assert!(took < Duration::from_secs(2), "open streams held shutdown for {took:?}");

    // The slow consumer still receives what was matched before shutdown,
    // then `shutdown`, then a clean end of the response. What was matched
    // is what its connection could buffer (REL-18 caps the write buffer at
    // `server.connection_buffer`), so it is an in-order prefix of the 300;
    // the rest stays queryable.
    let mut delivered = 0;
    let last = loop {
        match slow.next_or_end().await {
            Some((k, d)) if k == "event" => {
                let message = serde_json::from_str::<Value>(&d).unwrap()["message"].clone();
                assert_eq!(message, format!("live-{delivered}"), "events arrive in order with none skipped");
                delivered += 1;
            }
            Some((k, d)) => break (k, d),
            None => panic!("stream ended without a shutdown event"),
        }
    };
    assert!((1..=300).contains(&delivered), "{delivered} events delivered");
    assert_eq!(last, ("shutdown".to_string(), "{}".to_string()));
    assert_eq!(slow.retry, Some(2000), "clients are told when to reconnect");
    assert_eq!(slow.next_or_end().await, None);
    loop {
        match errors.next_or_end().await {
            Some((k, _)) if k == "event" => {}
            Some((k, _)) => assert_eq!(k, "shutdown"),
            None => break,
        }
    }

    // What EventSource does after `retry`: reconnect to the same URL.
    let s = TestServer::launch_at(config, dir, &addr).await;
    let mut again = Sse::open(&s, "").await;
    s.ingest(json!({"message": "after restart"})).await;
    let (k, d) = again.next().await;
    assert_eq!(k, "event");
    assert_eq!(serde_json::from_str::<Value>(&d).unwrap()["message"], "after restart");
    drop(again);
    s.stop().await;
}

/// The real binary, stopped with SIGTERM.
#[cfg(unix)]
mod process {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// An ingest request whose body is half sent: admitted, then waiting.
    async fn half_sent(p: &Process, body: &str) -> tokio::net::TcpStream {
        let addr = p.url.trim_start_matches("http://");
        let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "POST /api/v1/events HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            p.api_key,
            body.len()
        );
        tcp.write_all(head.as_bytes()).await.unwrap();
        tcp.write_all(&body.as_bytes()[..body.len() / 2]).await.unwrap();
        tcp.flush().await.unwrap();
        // Wait until the server has admitted it.
        let deadline = Instant::now() + Duration::from_secs(10);
        while p.get("/api/v1/system/info").await["ingest"]["inFlight"] != 1 {
            assert!(Instant::now() < deadline, "request admitted");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tcp
    }

    async fn ingest(p: &Process, tag: &str) {
        let r = p.client.post(format!("{}/api/v1/events", p.url)).json(&batch(tag, 4)).send().await.unwrap();
        assert_eq!(r.status(), 200);
    }

    async fn messages(p: &Process) -> Vec<String> {
        let v: Value = p
            .client
            .post(format!("{}/api/v1/query/logs", p.url))
            .json(&json!({"query": "service = drain", "limit": 1000}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut m: Vec<String> =
            v["events"].as_array().unwrap().iter().map(|e| e["message"].as_str().unwrap().to_string()).collect();
        m.sort();
        m
    }

    fn tagged(tags: &[&str]) -> Vec<String> {
        let mut m: Vec<String> = tags.iter().flat_map(|t| (0..4).map(move |i| format!("{t}-{i}"))).collect();
        m.sort();
        m
    }

    // AC1 + AC2 end to end: SIGTERM waits for the request in flight, which
    // is acknowledged, then exits 0; after a restart every acknowledged
    // event is queryable exactly once.
    #[tokio::test(flavor = "multi_thread")]
    async fn sigterm_drains_in_flight_ingest_and_exits_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Process::start(dir.path(), &[]).await;
        ingest(&p, "acked").await;
        let body = batch("drained", 4).to_string();
        let mut tcp = half_sent(&p, &body).await;

        p.terminate();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(p.exited().is_none(), "vyrtel waits for the request in flight");
        tcp.write_all(&body.as_bytes()[body.len() / 2..]).await.unwrap();
        let mut response = String::new();
        tcp.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let status = p.wait_exit(Duration::from_secs(10)).await;
        assert!(status.success(), "{status:?}: {}", p.stderr());

        let p = Process::start(dir.path(), &[]).await;
        assert_eq!(messages(&p).await, tagged(&["acked", "drained"]));
        let storage = p.get("/api/v1/system/storage").await;
        assert_eq!(storage["signals"]["logs"]["recovery"]["quarantined"], json!([]), "a clean stop damages nothing");
    }

    // AC3 + TC-03: past the deadline the process exits 1 within it and
    // prints which work was unfinished; acknowledged events survive.
    #[tokio::test(flavor = "multi_thread")]
    async fn sigterm_past_the_deadline_exits_with_a_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Process::start(dir.path(), &[("VYRTEL_SERVER_SHUTDOWN_TIMEOUT", "1s")]).await;
        ingest(&p, "acked").await;
        let _tcp = half_sent(&p, &batch("unfinished", 4).to_string()).await;

        let began = Instant::now();
        p.terminate();
        let status = p.wait_exit(Duration::from_secs(10)).await;
        let took = began.elapsed();
        assert_eq!(status.code(), Some(1), "{}", p.stderr());
        assert!((Duration::from_millis(900)..Duration::from_secs(4)).contains(&took), "exited after {took:?}");
        let stderr = p.stderr();
        for part in [
            "server.shutdown_timeout (1s) passed while waiting for in-flight requests",
            "1 ingest request not acknowledged (the client gets no response and should retry)",
        ] {
            assert!(stderr.contains(part), "{part:?} in {stderr}");
        }

        let p = Process::start(dir.path(), &[]).await;
        assert_eq!(messages(&p).await, tagged(&["acked"]));
    }

    // TC-03: a repeated SIGTERM ends the drain at once.
    #[tokio::test(flavor = "multi_thread")]
    async fn repeated_sigterm_stops_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Process::start(dir.path(), &[("VYRTEL_SERVER_SHUTDOWN_TIMEOUT", "60s")]).await;
        let _tcp = half_sent(&p, &batch("unfinished", 4).to_string()).await;
        p.terminate();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(p.exited().is_none());
        let began = Instant::now();
        p.terminate();
        let status = p.wait_exit(Duration::from_secs(5)).await;
        assert!(began.elapsed() < Duration::from_secs(2), "{:?}", began.elapsed());
        assert_eq!(status.code(), Some(1));
        assert!(p.stderr().contains("shutdown incomplete: a second stop signal arrived"), "{}", p.stderr());
    }
}
