//! REL-18: the HTTP transport boundary. Connections beyond
//! `server.max_connections` wait briefly and are then refused, slow and idle
//! clients are closed on their deadlines, buffers are capped, and every way
//! a connection can end gives its slot back.
//!
//! Tests synchronise on the transport's own counters
//! (`AppState::transport`), not on sleeps; the only waits are for the
//! deadlines under test, set to a few hundred milliseconds.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use server::transport::{BUSY_RESPONSE, PENDING_LIMIT, PENDING_TIMEOUT, TransportSnapshot};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::common::*;

fn snap(s: &TestServer) -> TransportSnapshot {
    s.state().transport.snapshot()
}

fn addr(s: &TestServer) -> String {
    s.url.trim_start_matches("http://").to_string()
}

async fn connect(s: &TestServer) -> TcpStream {
    TcpStream::connect(addr(s)).await.unwrap()
}

/// Read until the server closes the connection (EOF or reset) and return
/// what arrived. Panics if it is still open after `within`.
async fn read_to_close(tcp: &mut TcpStream, within: Duration) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 << 10];
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, tcp.read(&mut buf)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return out,
            Ok(Ok(n)) => out.extend_from_slice(&buf[..n]),
            Err(_) => panic!("connection still open after {within:?}; read {:?}", String::from_utf8_lossy(&out)),
        }
    }
}

/// Read one complete response with a `Content-Length` body, leaving the
/// connection open.
async fn read_response(tcp: &mut TcpStream) -> (u16, String) {
    let mut data = Vec::new();
    let mut buf = [0u8; 8 << 10];
    loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..end]).to_string();
            let length: usize = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").map(|n| n.trim().to_string()))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                let status = head[9..12].parse().unwrap();
                return (status, String::from_utf8_lossy(&data[end + 4..end + 4 + length]).to_string());
            }
        }
        let n = tokio::time::timeout(Duration::from_secs(10), tcp.read(&mut buf)).await.unwrap().unwrap();
        assert!(n > 0, "connection closed mid-response: {:?}", String::from_utf8_lossy(&data));
        data.extend_from_slice(&buf[..n]);
    }
}

/// Close with a reset (RST) instead of a FIN when dropped.
#[allow(deprecated)] // A zero linger never blocks on drop, which is what the deprecation is about.
fn reset_on_drop(tcp: &TcpStream) {
    tcp.set_linger(Some(Duration::ZERO)).unwrap();
}

fn get(path: &str, close: bool) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: test\r\n{}\r\n", if close { "Connection: close\r\n" } else { "" })
}

/// A refused connection gets the fixed 503, or part of it, or (if the
/// socket did not take it, or the close reset the connection before it was
/// read) only a closed connection; never anything else.
fn assert_refused(got: &[u8]) {
    assert!(BUSY_RESPONSE.starts_with(got), "{}", String::from_utf8_lossy(got));
}

/// The server still ingests and queries normally.
async fn assert_healthy(s: &TestServer, tag: &str) {
    s.ingest(json!([{"message": tag, "service": "transport"}])).await;
    assert!(s.messages("service = transport").await.contains(&tag.to_string()));
    assert_eq!(ingest_in_flight(s), 0);
}

// AC1, TC1: C−1 and C connections are admitted, C+1 waits and gets the
// first freed slot, waiting is capped at PENDING_LIMIT and PENDING_TIMEOUT,
// and refusals are a fixed 503 or a close.
#[tokio::test(flavor = "multi_thread")]
async fn connections_beyond_the_limit_wait_briefly_then_are_refused() {
    const C: usize = 4;
    let s = TestServer::start_with(|c| {
        c.server.max_connections = C;
        c.query.max_live_streams = 1;
    })
    .await;
    assert_eq!(snap(&s).max_connections, C);

    let mut idle = Vec::new();
    for i in 1..=C {
        idle.push(connect(&s).await);
        wait_until("connection admitted", || snap(&s).active == i).await;
    }
    let mut waiting = connect(&s).await;
    wait_until("C+1 waits", || snap(&s).pending == 1).await;
    assert_eq!((snap(&s).active, snap(&s).queued), (C, 1));

    // The first freed slot goes to it, and it is served.
    drop(idle.pop());
    wait_until("waiting connection admitted", || snap(&s).pending == 0 && snap(&s).admitted == C as u64 + 1).await;
    waiting.write_all(get("/health", true).as_bytes()).await.unwrap();
    let got = read_to_close(&mut waiting, Duration::from_secs(5)).await;
    assert!(got.starts_with(b"HTTP/1.1 200 OK"), "{}", String::from_utf8_lossy(&got));
    wait_until("its slot freed", || snap(&s).active == C - 1).await;

    // Full again, with every waiting place taken: the next one is refused
    // at once.
    idle.push(connect(&s).await);
    wait_until("full", || snap(&s).active == C).await;
    let mut queue = Vec::new();
    for _ in 0..PENDING_LIMIT {
        queue.push(connect(&s).await);
    }
    wait_until("queue full", || snap(&s).pending == PENDING_LIMIT).await;
    let began = Instant::now();
    let mut over = connect(&s).await;
    assert_refused(&read_to_close(&mut over, Duration::from_secs(1)).await);
    assert!(began.elapsed() < Duration::from_secs(1), "refused at once, took {:?}", began.elapsed());
    assert_eq!(snap(&s).refused, 1);

    // The waiting ones are refused once PENDING_TIMEOUT passes.
    for tcp in &mut queue {
        assert_refused(&read_to_close(tcp, PENDING_TIMEOUT + Duration::from_secs(5)).await);
    }
    let t = snap(&s);
    assert_eq!((t.pending, t.refused, t.active), (0, 1 + PENDING_LIMIT as u64, C));

    drop(idle);
    wait_until("every slot free", || snap(&s).active == 0).await;
    assert_healthy(&s, "after saturation").await;
    s.stop().await;
}

// AC1, AC6 (functional part), TC1: over a thousand simultaneous attempts
// and repeated reconnect waves never hold more than C slots or
// PENDING_LIMIT waiting places, every attempt is accounted for, and the
// server returns to idle.
#[tokio::test(flavor = "multi_thread")]
async fn a_thousand_simultaneous_attempts_never_exceed_the_limits() {
    const C: usize = 16;
    const ATTEMPTS: usize = 1000;
    let s = TestServer::start_with(|c| {
        c.server.max_connections = C;
        c.server.header_timeout = Duration::from_millis(500);
        c.query.max_live_streams = 1;
    })
    .await;
    let state = s.state().clone();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let sampler = tokio::spawn(async move {
        let (mut active, mut pending) = (0, 0);
        loop {
            let t = state.transport.snapshot();
            active = active.max(t.active);
            pending = pending.max(t.pending);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                _ = &mut stop_rx => return (active, pending),
            }
        }
    });

    let mut total = 0u64;
    for (wave, attempts) in [ATTEMPTS, 300, 300, 300].into_iter().enumerate() {
        let target = addr(&s);
        let clients: Vec<_> = (0..attempts)
            .map(|i| {
                let target = target.clone();
                tokio::spawn(async move {
                    let Ok(mut tcp) = TcpStream::connect(&target).await else { return None };
                    // Half send a request and keep the connection alive;
                    // half send nothing at all.
                    if i % 2 == 0 {
                        let _ = tcp.write_all(get("/health", false).as_bytes()).await;
                    }
                    let mut got = Vec::new();
                    let _ = tokio::time::timeout(Duration::from_secs(20), tcp.read_to_end(&mut got)).await;
                    Some(got)
                })
            })
            .collect();
        let mut served = 0;
        for c in clients {
            let got = c.await.unwrap().expect("connect succeeds: the backlog holds every attempt");
            if got.starts_with(b"HTTP/1.1 200") {
                served += 1;
            } else {
                assert_refused(&got);
            }
        }
        total += attempts as u64;
        wait_until("wave settles", || {
            let t = snap(&s);
            t.active == 0 && t.pending == 0 && t.admitted + t.refused == total
        })
        .await;
        let t = snap(&s);
        eprintln!(
            "REL-18 wave {wave}: {attempts} attempts, {served} answered; admitted {} (queued {}), refused {}, \
             header timeouts {}",
            t.admitted, t.queued, t.refused, t.header_timeouts
        );
        assert!(served > 0, "healthy clients within capacity were served");
    }
    let _ = stop_tx.send(());
    let (max_active, max_pending) = sampler.await.unwrap();
    eprintln!("REL-18 peaks: {max_active} active of {C}, {max_pending} waiting of {PENDING_LIMIT}");
    assert!(max_active <= C, "{max_active} active");
    assert!(max_pending <= PENDING_LIMIT, "{max_pending} waiting");
    assert_eq!(snap(&s).accept_errors, 0);
    assert_healthy(&s, "after waves").await;
    s.stop().await;
}

// AC4, TC2: before a request head is complete nothing is routed. Idle,
// byte-at-a-time and idle keep-alive connections close on
// `server.header_timeout`; heads larger than the buffer or with too many
// headers get 431.
#[tokio::test(flavor = "multi_thread")]
async fn incomplete_idle_and_oversized_request_heads_are_closed() {
    let timeout = Duration::from_millis(300);
    let s = TestServer::start_with(|c| {
        c.server.header_timeout = Duration::from_millis(300);
        c.server.connection_buffer = server::config::ByteSize(8 << 10);
    })
    .await;

    // Accepted, never sends anything.
    let began = Instant::now();
    let mut silent = connect(&s).await;
    assert!(read_to_close(&mut silent, Duration::from_secs(5)).await.is_empty());
    let took = began.elapsed();
    assert!(took >= timeout - Duration::from_millis(50) && took < Duration::from_secs(3), "{took:?}");
    wait_until("header timeout counted", || snap(&s).header_timeouts == 1).await;

    // A head sent one byte at a time never completes.
    let mut trickle = connect(&s).await;
    let (mut rd, mut wr) = trickle.split();
    let began = Instant::now();
    let writer = async {
        for b in b"GET /health HTTP/1.1\r\nHost: test\r\nX-Slow: ".iter().chain(std::iter::repeat(&b'a')) {
            if wr.write_all(&[*b]).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let reader = async {
        let mut got = Vec::new();
        let _ = rd.read_to_end(&mut got).await;
        got
    };
    let got = tokio::select! {
        got = reader => got,
        _ = writer => panic!("writer finished first"),
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("trickled head not closed"),
    };
    assert!(!String::from_utf8_lossy(&got).contains("200 OK"), "never routed");
    assert!(began.elapsed() < Duration::from_secs(3));
    wait_until("second header timeout", || snap(&s).header_timeouts == 2).await;
    drop(trickle);

    // Keep-alive: reused for a second request, then closed once idle.
    let mut keep = connect(&s).await;
    for _ in 0..2 {
        keep.write_all(get("/health", false).as_bytes()).await.unwrap();
        assert_eq!(read_response(&mut keep).await.0, 200);
    }
    assert!(read_to_close(&mut keep, Duration::from_secs(5)).await.is_empty());
    wait_until("idle keep-alive closed", || snap(&s).header_timeouts == 3).await;

    // Larger than server.connection_buffer.
    let mut big = connect(&s).await;
    let head = format!("GET /health HTTP/1.1\r\nHost: test\r\nX-Big: {}\r\n\r\n", "a".repeat(16 << 10));
    let _ = big.write_all(head.as_bytes()).await;
    let got = String::from_utf8_lossy(&read_to_close(&mut big, Duration::from_secs(5)).await).to_string();
    assert!(got.starts_with("HTTP/1.1 431 "), "{got}");

    // More than MAX_HEADERS headers.
    let mut many = connect(&s).await;
    let mut head = String::from("GET /health HTTP/1.1\r\nHost: test\r\n");
    for i in 0..server::transport::MAX_HEADERS {
        head.push_str(&format!("X-H{i}: v\r\n"));
    }
    head.push_str("\r\n");
    many.write_all(head.as_bytes()).await.unwrap();
    let got = String::from_utf8_lossy(&read_to_close(&mut many, Duration::from_secs(5)).await).to_string();
    assert!(got.starts_with("HTTP/1.1 431 "), "{got}");
    wait_until("protocol errors counted", || snap(&s).protocol_errors == 2).await;

    wait_until("every slot free", || snap(&s).active == 0).await;
    assert_healthy(&s, "after heads").await;
    s.stop().await;
}

// AC4, TC3: a body that does not arrive within `server.body_timeout` gets
// 408 and the connection closes; the ingest slot it held is released.
#[tokio::test(flavor = "multi_thread")]
async fn slow_request_bodies_get_408_and_release_their_slots() {
    let s = TestServer::start_with(|c| c.server.body_timeout = Duration::from_millis(300)).await;
    for (path, n) in [("/api/v1/events", 1), ("/api/v1/query/logs", 2)] {
        let began = Instant::now();
        let mut r = RawRequest::open(&s, path, &[("Content-Type", "application/json")], 1000).await;
        r.send(b"[{\"message\":").await;
        let (status, text) = r.response().await;
        assert_eq!(status, 408, "{path}: {text}");
        let body = text.split_once("\r\n\r\n").unwrap().1;
        let v: Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["error"]["code"], "request_timeout", "{path}");
        assert!(v["error"]["message"].as_str().unwrap().contains("server.body_timeout"), "{body}");
        assert!(began.elapsed() < Duration::from_secs(3), "{path}: {:?}", began.elapsed());
        wait_until("body timeout counted", || snap(&s).body_timeouts == n).await;
    }
    wait_until("slots free", || snap(&s).active == 0 && ingest_in_flight(&s) == 0).await;
    assert_healthy(&s, "after slow bodies").await;
    s.stop().await;
}

// AC4, TC3: a client that sends pipelined queries and never reads the
// responses is closed once a write has been blocked for
// `server.write_timeout`.
#[tokio::test(flavor = "multi_thread")]
async fn clients_that_stop_reading_are_closed_on_the_write_timeout() {
    let s = TestServer::start_with(|c| c.server.write_timeout = Duration::from_millis(300)).await;
    let pad = "x".repeat(1000);
    for b in 0..10 {
        let batch: Vec<Value> = (0..100).map(|i| json!({"message": format!("big-{b}-{i}"), "pad": pad})).collect();
        s.ingest(Value::Array(batch)).await;
    }
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut tcp = socket.connect(addr(&s).parse().unwrap()).await.unwrap();
    let body = r#"{"query": "", "limit": 1000}"#;
    let request = format!(
        "POST /api/v1/query/logs HTTP/1.1\r\nHost: test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let began = Instant::now();
    let writer = tokio::spawn(async move {
        // ~1 MB of response per request; never read.
        for _ in 0..200 {
            if tcp.write_all(request.as_bytes()).await.is_err() {
                break;
            }
        }
        tcp
    });
    wait_until("write timeout", || snap(&s).write_timeouts == 1).await;
    wait_until("slot freed", || snap(&s).active == 0).await;
    let took = began.elapsed();
    eprintln!("REL-18 stalled reader closed after {took:?}");
    drop(writer.await.unwrap());
    assert!(s.query("").await["events"].as_array().is_some_and(|e| !e.is_empty()));
    s.stop().await;
}

// AC2, TC3: pipelined requests are answered one at a time, in order, on
// one connection; HTTP/2 is not served and an h2c upgrade is ignored.
#[tokio::test(flavor = "multi_thread")]
async fn pipelined_requests_are_served_in_order_and_http2_is_not_served() {
    let s = TestServer::start().await;
    let mut tcp = connect(&s).await;
    let mut burst = String::new();
    for _ in 0..19 {
        burst.push_str(&get("/health", false));
    }
    burst.push_str(&get("/ready", true));
    tcp.write_all(burst.as_bytes()).await.unwrap();
    let got = String::from_utf8_lossy(&read_to_close(&mut tcp, Duration::from_secs(10)).await).to_string();
    assert_eq!(got.matches("HTTP/1.1 200 OK").count(), 20, "{got}");
    assert!(got.trim_end().ends_with(r#"{"status":"ready","storageWritable":true}"#), "the last answer is last");
    assert_eq!(snap(&s).admitted, 1, "one connection");

    // Prior-knowledge HTTP/2 is a protocol error, not a second protocol.
    let mut h2 = connect(&s).await;
    h2.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n").await.unwrap();
    let got = read_to_close(&mut h2, Duration::from_secs(5)).await;
    assert!(!String::from_utf8_lossy(&got).contains("200"), "{:?}", String::from_utf8_lossy(&got));
    wait_until("protocol error counted", || snap(&s).protocol_errors == 1).await;

    // An h2c upgrade request is answered as plain HTTP/1.1.
    let mut up = connect(&s).await;
    up.write_all(b"GET /health HTTP/1.1\r\nHost: test\r\nConnection: Upgrade, HTTP2-Settings, close\r\nUpgrade: h2c\r\nHTTP2-Settings: AAMAAABkAARAAAAAAAIAAAAA\r\n\r\n")
        .await
        .unwrap();
    let got = String::from_utf8_lossy(&read_to_close(&mut up, Duration::from_secs(5)).await).to_string();
    assert!(got.starts_with("HTTP/1.1 200 OK"), "{got}");
    s.stop().await;
}

// AC5, TC4: resets, EOF mid-head, malformed input, handler failures and
// abandoned responses all give their slot back while saturated; a healthy
// client is served afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn every_way_a_connection_ends_releases_its_slot() {
    const C: usize = 2;
    let s = TestServer::start_with(|c| {
        c.server.max_connections = C;
        c.query.max_live_streams = 1;
    })
    .await;
    let settle = |what: &'static str| {
        let s = &s;
        async move { wait_until(what, || snap(s).active == 0 && snap(s).pending == 0).await }
    };

    // Peer reset (RST) while holding every slot, with a waiter behind it.
    let mut held = Vec::new();
    for _ in 0..C {
        let tcp = connect(&s).await;
        reset_on_drop(&tcp);
        held.push(tcp);
    }
    wait_until("saturated", || snap(&s).active == C).await;
    let mut waiter = connect(&s).await;
    wait_until("waiting", || snap(&s).pending == 1).await;
    drop(held);
    waiter.write_all(get("/health", true).as_bytes()).await.unwrap();
    assert!(read_to_close(&mut waiter, Duration::from_secs(5)).await.starts_with(b"HTTP/1.1 200"));
    settle("after reset").await;

    // EOF in the middle of a head.
    for _ in 0..C {
        let mut tcp = connect(&s).await;
        tcp.write_all(b"GET /hea").await.unwrap();
        tcp.shutdown().await.unwrap();
        let _ = read_to_close(&mut tcp, Duration::from_secs(5)).await;
    }
    settle("after EOF").await;

    // Malformed input: 400, then closed.
    let mut bad = connect(&s).await;
    bad.write_all(b"NOT HTTP AT ALL\r\n\r\n").await.unwrap();
    let got = String::from_utf8_lossy(&read_to_close(&mut bad, Duration::from_secs(5)).await).to_string();
    assert!(got.starts_with("HTTP/1.1 400 "), "{got}");
    settle("after malformed").await;

    // A handler error keeps the connection usable.
    let mut tcp = connect(&s).await;
    let body = "{not json";
    tcp.write_all(
        format!(
            "POST /api/v1/query/logs HTTP/1.1\r\nHost: test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    assert_eq!(read_response(&mut tcp).await.0, 400);
    tcp.write_all(get("/health", true).as_bytes()).await.unwrap();
    assert!(read_to_close(&mut tcp, Duration::from_secs(5)).await.starts_with(b"HTTP/1.1 200"));
    settle("after handler error").await;

    // A response abandoned mid-way (the client sends a query and leaves).
    for _ in 0..C {
        let mut tcp = connect(&s).await;
        let body = r#"{"query": ""}"#;
        tcp.write_all(
            format!(
                "POST /api/v1/query/logs HTTP/1.1\r\nHost: test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        reset_on_drop(&tcp);
        drop(tcp);
    }
    settle("after abandoned responses").await;

    assert_healthy(&s, "after every ending").await;
    s.stop().await;
}

// AC5, TC4: a stop request while every slot is held and connections are
// waiting closes the idle ones, refuses the waiting ones, and the drain
// finishes well within its deadline.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_while_saturated_is_bounded() {
    const C: usize = 2;
    let mut s = TestServer::start_with(|c| {
        c.server.max_connections = C;
        c.server.header_timeout = Duration::from_secs(60);
        c.query.max_live_streams = 1;
    })
    .await;
    let mut idle = Vec::new();
    for _ in 0..C {
        idle.push(connect(&s).await);
    }
    wait_until("saturated", || snap(&s).active == C).await;
    let mut waiting = connect(&s).await;
    wait_until("waiting", || snap(&s).pending == 1).await;

    let began = Instant::now();
    s.begin_stop();
    let (r, _, _) = s.stopped().await;
    r.unwrap();
    let took = began.elapsed();
    assert!(took < Duration::from_secs(2), "took {took:?}");
    for tcp in &mut idle {
        assert!(read_to_close(tcp, Duration::from_secs(1)).await.is_empty(), "idle connections just close");
    }
    assert_refused(&read_to_close(&mut waiting, Duration::from_secs(1)).await);
}

// AC1, TC5: listeners served by one process share one budget.
#[tokio::test(flavor = "multi_thread")]
async fn listeners_share_one_connection_budget() {
    const C: usize = 3;
    let s = TestServer::start_with(|c| {
        c.server.max_connections = C;
        c.query.max_live_streams = 1;
    })
    .await;
    let second = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second_addr = second.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server::transport::serve(
        second,
        server::routes::router(s.state().clone()),
        s.state().transport.clone(),
        async move {
            let _ = stop_rx.await;
        },
    ));

    let mut held = vec![connect(&s).await, connect(&s).await];
    held.push(TcpStream::connect(second_addr).await.unwrap());
    wait_until("both listeners count", || snap(&s).active == C).await;
    let mut over = TcpStream::connect(second_addr).await.unwrap();
    wait_until("the second listener's extra connection waits", || snap(&s).pending == 1).await;
    drop(held.pop());
    over.write_all(get("/health", true).as_bytes()).await.unwrap();
    assert!(read_to_close(&mut over, Duration::from_secs(5)).await.starts_with(b"HTTP/1.1 200"));

    drop(held);
    let _ = stop_tx.send(());
    serving.await.unwrap().unwrap();
    wait_until("every slot free", || snap(&s).active == 0).await;
    s.stop().await;
}

// AC2: the limits are reported where operators look for them.
#[tokio::test(flavor = "multi_thread")]
async fn system_info_reports_connection_limits_and_outcomes() {
    let s = TestServer::start_with(|c| {
        c.server.max_connections = 32;
        c.server.connection_buffer = server::config::ByteSize(16 << 10);
    })
    .await;
    let v = s.get_ok("/api/v1/system/info").await;
    let c = &v["connections"];
    assert_eq!(c["maxConnections"], 32);
    assert_eq!(c["active"], 1, "this request's own connection");
    assert_eq!(c["maxPending"], PENDING_LIMIT);
    assert_eq!(c["connectionBufferBytes"], 16 << 10);
    assert_eq!(c["estimatedPeakBytes"], s.config.transport_limits().estimated_peak());
    for key in ["admitted", "queued", "refused", "headerTimeouts", "bodyTimeouts", "writeTimeouts", "protocolErrors"] {
        assert!(c[key].is_u64(), "{key}: {c}");
    }
    let config = s.get_ok("/api/v1/system/config").await;
    assert_eq!(
        config["config"]["server"],
        json!({
            "bind": "127.0.0.1:0",
            "shutdown_timeout": "30s",
            "max_connections": 32,
            "header_timeout": "10s",
            "body_timeout": "1m",
            "write_timeout": "30s",
            "connection_buffer": 16384,
        })
    );
    s.stop().await;
}
