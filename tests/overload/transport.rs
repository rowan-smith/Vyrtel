//! Heap held by the HTTP transport (REL-18 AC2, AC6, TC1, TC6).
//!
//! Fills every connection slot with idle connections, then with request
//! heads that fill each read buffer and never end, then floods the full
//! server with more connection attempts than it has slots and waiting
//! places, and finally closes everything. Clients run on [`client_runtime`],
//! so only the server's allocations are counted. Measures:
//!
//! * heap per idle connection and per connection with a full read buffer,
//!   which `transport::CONNECTION_OVERHEAD` and the documented estimate are
//!   based on;
//! * peak heap growth and runtime tasks during the flood, which must stay
//!   within `TransportLimits::estimated_peak` and `max_connections +
//!   PENDING_LIMIT` tasks;
//! * heap retained after everything closed, which must return to the
//!   starting level.

use std::sync::atomic::Ordering;
use std::time::Duration;

use server::transport::{CONNECTION_OVERHEAD, PENDING_LIMIT};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::*;

/// Heap a connection may keep after closing, in total: allocator and
/// runtime bookkeeping that is reused rather than freed.
const RETAINED_TOLERANCE: usize = 512 << 10;

fn heap() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// Let the server's tasks run, then read the heap.
async fn settled_heap() -> usize {
    tokio::time::sleep(Duration::from_millis(200)).await;
    heap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_heap_stays_within_the_transport_estimate() {
    let _serial = SERIAL.lock().await;
    const C: usize = 200;
    const FLOOD: usize = 1000;
    let s = TestServer::start_with(|c| {
        c.server.max_connections = C;
        c.server.header_timeout = Duration::from_secs(60);
        c.server.write_timeout = Duration::from_secs(5);
        c.alerts.enabled = false;
        c.query.max_live_streams = 1;
    })
    .await;
    let limits = s.config.transport_limits();
    let buffer = limits.connection_buffer;
    let addr = s.url.trim_start_matches("http://").to_string();
    let clients = client_runtime();
    let transport = s.state().transport.clone();

    // One request first, so state initialised on first use is not charged
    // to the connections measured below.
    let target = addr.clone();
    clients
        .spawn(async move {
            let mut tcp = TcpStream::connect(&target).await.unwrap();
            tcp.write_all(b"GET /health HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut out = Vec::new();
            tcp.read_to_end(&mut out).await.unwrap();
        })
        .await
        .unwrap();
    wait_until("warm-up closed", || transport.active() == 0).await;
    let tasks_before = tokio::runtime::Handle::current().metrics().num_alive_tasks();
    let base = settled_heap().await;

    // Every slot held by a connection that has sent nothing.
    let target = addr.clone();
    let mut held = clients
        .spawn(async move {
            let mut v = Vec::with_capacity(C);
            for _ in 0..C {
                v.push(TcpStream::connect(&target).await.unwrap());
            }
            v
        })
        .await
        .unwrap();
    wait_until("every slot held", || transport.active() == C).await;
    let idle = (settled_heap().await - base) / C;

    // Each sends a head that fills its read buffer and never ends.
    let head = format!("GET /health HTTP/1.1\r\nHost: t\r\nX-Pad: {}", "a".repeat(buffer - 1024));
    held = clients
        .spawn(async move {
            for tcp in &mut held {
                tcp.write_all(head.as_bytes()).await.unwrap();
            }
            held
        })
        .await
        .unwrap();
    let full_target = base + C * buffer / 2;
    wait_until("heads buffered", || heap() >= full_target).await;
    let full = (settled_heap().await - base) / C;

    // More attempts than slots and waiting places, while full.
    let start = reset_peak();
    let target = addr.clone();
    let flood = clients.spawn(async move {
        let mut v = Vec::with_capacity(FLOOD);
        for _ in 0..FLOOD {
            if let Ok(tcp) = TcpStream::connect(&target).await {
                v.push(tcp);
            }
        }
        v
    });
    let flooded = flood.await.unwrap();
    assert_eq!(flooded.len(), FLOOD, "the backlog took every attempt");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let t = transport.snapshot();
        // Every slot stays held, so each attempt is waiting or was refused
        // (at once, or after waiting).
        assert!(t.pending <= PENDING_LIMIT && t.active == C, "{t:?}");
        if t.refused + t.pending as u64 == FLOOD as u64 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "flood not accounted for: {t:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let tasks = tokio::runtime::Handle::current().metrics().num_alive_tasks().saturating_sub(tasks_before);
    let t = transport.snapshot();
    let peak = PEAK.load(Ordering::Relaxed).max(start) - base;

    // Everything closes; the heap returns to where it started.
    clients.spawn(async move { drop((held, flooded)) }).await.unwrap();
    wait_until("idle again", || transport.active() == 0 && transport.pending() == 0).await;
    let retained = settled_heap().await.saturating_sub(base);

    // Every slot held by a client that pipelines requests and never reads
    // a response: each connection's read buffer fills with unread requests
    // and its write buffer with unsent responses.
    let write_timeouts = transport.snapshot().write_timeouts;
    let target = addr.clone();
    let stalled = clients
        .spawn(async move {
            let mut v = Vec::with_capacity(C);
            for _ in 0..C {
                let socket = tokio::net::TcpSocket::new_v4().unwrap();
                socket.set_recv_buffer_size(4096).unwrap();
                v.push(socket.connect(target.parse().unwrap()).await.unwrap());
            }
            let request = b"GET /api/v1/system/config HTTP/1.1\r\nHost: t\r\n\r\n".repeat(16);
            let writers: Vec<_> = v
                .into_iter()
                .map(|mut tcp| {
                    let request = request.clone();
                    tokio::spawn(async move {
                        // Until the server stops reading (its buffers are
                        // full) or closes the connection.
                        while let Ok(Ok(())) =
                            tokio::time::timeout(Duration::from_secs(1), tcp.write_all(&request)).await
                        {
                        }
                        tcp
                    })
                })
                .collect();
            let mut v = Vec::with_capacity(C);
            for w in writers {
                v.push(w.await.unwrap());
            }
            v
        })
        .await
        .unwrap();
    let stalled_per = (settled_heap().await.saturating_sub(base)) / C;
    wait_until("stalled writes time out", || transport.snapshot().write_timeouts >= write_timeouts + C as u64).await;
    wait_until("idle after write timeouts", || transport.active() == 0).await;
    clients.spawn(async move { drop(stalled) }).await.unwrap();
    let retained_after_stall = settled_heap().await.saturating_sub(base);
    clients.shutdown_background();

    eprintln!(
        "REL-18 transport heap: {C} slots, {} buffer; {:.1} KB per idle connection, {:.1} KB per full read \
         buffer, {:.1} KB per stalled reader; flood of {FLOOD}: {} waiting, {} refused, {tasks} new tasks; peak \
         growth {:.2} MB of {:.2} MB estimated; {:.1} KB retained after close, {:.1} KB after write timeouts",
        human(buffer),
        idle as f64 / 1024.0,
        full as f64 / 1024.0,
        stalled_per as f64 / 1024.0,
        t.pending,
        t.refused,
        peak as f64 / MB,
        limits.estimated_peak() as f64 / MB,
        retained as f64 / 1024.0,
        retained_after_stall as f64 / 1024.0,
    );
    let per_connection = limits.estimated_per_connection() as usize;
    assert!(idle <= 8192 + CONNECTION_OVERHEAD as usize, "idle connection holds {idle} bytes");
    assert!(full <= per_connection, "full read buffer: {full} bytes, estimate {per_connection}");
    assert!(full > buffer / 2, "the head really filled the buffer ({full} bytes)");
    assert!(stalled_per <= per_connection, "stalled reader: {stalled_per} bytes, estimate {per_connection}");
    assert!(peak as u64 <= limits.estimated_peak(), "peak {peak} over estimate {}", limits.estimated_peak());
    assert_eq!(t.active, C, "never more than max_connections");
    assert!(tasks <= C + PENDING_LIMIT + 16, "{tasks} tasks for {C} slots");
    assert!(retained <= RETAINED_TOLERANCE, "{retained} bytes retained after every connection closed");
    assert!(retained_after_stall <= RETAINED_TOLERANCE, "{retained_after_stall} bytes retained after write timeouts");

    // A healthy client is served once capacity is back.
    assert_eq!(post(&s, &bytes_of(b"{\"message\":\"after\"}\n".to_vec()), false).await, 200);
    s.stop().await;
}

fn human(n: usize) -> String {
    server::config::human_bytes(n as u64)
}
