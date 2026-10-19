//! The HTTP transport boundary (REL-18): every byte, task and file
//! descriptor a client can make Vyrtel hold before a handler applies its own
//! admission (ingest, query, live tail) is bounded here.
//!
//! * **Connections.** At most `server.max_connections` are admitted at once,
//!   across every listener this process serves. A slot is taken before the
//!   connection's task is spawned or any buffer is allocated, and it is
//!   released when the task ends, however it ends: EOF, reset, timeout,
//!   malformed input, handler error, write blockage or shutdown.
//! * **Waiting.** A connection that arrives while every slot is taken waits
//!   for one, holding only its socket and a small task, for at most
//!   [`PENDING_TIMEOUT`]; at most [`PENDING_LIMIT`] wait at once. Past
//!   either bound it is refused: Vyrtel writes a fixed
//!   `503 server_busy` response if the socket accepts it without blocking,
//!   then closes. The client may see the 503, or only a closed or reset
//!   connection; refusal never allocates or waits.
//! * **Buffers.** HTTP/1.1 only (HTTP/2 and upgrades are not served). One
//!   request at a time per connection, pipelined requests included; the read
//!   buffer (which also holds the request head) and the write buffer are
//!   each capped at `server.connection_buffer`, and a request head may carry
//!   at most [`MAX_HEADERS`] headers.
//! * **Deadlines.** `server.header_timeout` from when the connection is ready
//!   for a request (accepted, or idle after a response) until its head has
//!   arrived; it also ends idle keep-alive connections.
//!   `server.body_timeout` from the head until the end of the body.
//!   `server.write_timeout` for any single write that the client does not
//!   let complete (it stopped reading).
//!
//! The kernel's listen backlog (connections not yet accepted) is outside
//! this; see docs/configuration.md#connections-and-transport-limits.

use std::error::Error as StdError;
use std::future::Future;
use std::io;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use http_body::{Frame, SizeHint};
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::{Instant, Sleep};

use crate::config::ServerConfig;

/// Headers accepted in one request head (hyper's default, set explicitly).
/// More get `431` and the connection closes.
pub const MAX_HEADERS: usize = 100;

/// Connections waiting for a slot at once. Beyond this they are refused at
/// once.
pub const PENDING_LIMIT: usize = 64;

/// How long a connection waits for a slot before it is refused.
pub const PENDING_TIMEOUT: Duration = Duration::from_secs(2);

/// Pause after an accept error other than a connection-level one (usually
/// the process is out of file descriptors), so the loop does not spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(250);

/// Smallest `server.connection_buffer` (hyper's own minimum).
pub const MIN_CONNECTION_BUFFER: u64 = 8 << 10;

/// Largest `server.connection_buffer`: hyper's default read buffer limit,
/// which the ingest memory estimate assumes for connections draining a
/// rejected body (`config::INGEST_FIXED_OVERHEAD`).
pub const MAX_CONNECTION_BUFFER: u64 = 8192 + 4096 * 100;

/// Largest `server.max_connections`.
pub const MAX_CONNECTIONS_LIMIT: usize = 65_536;

/// Heap held by one admitted connection apart from its buffers: its task,
/// hyper's connection state, the per-request service and timers. Measured
/// by `tests/overload/transport.rs` (see
/// docs/testing.md#transport-limit-tests); used for the estimate only.
pub const CONNECTION_OVERHEAD: u64 = 16 << 10;

/// Allocated bytes per byte of `server.connection_buffer`, for both
/// buffers together. The read buffer holds at most `connection_buffer`
/// bytes, but growing to that limit can leave up to twice as much allocated
/// (hyper reserves ahead), so it counts twice. The write buffer holds
/// response heads and queues body chunks without copying them, so it
/// counts once. Measured by `tests/overload/transport.rs`: a full read
/// buffer of 64 KB held 1.75× its size plus overhead; a client that stopped
/// reading, far less.
pub const BUFFER_ALLOCATION_FACTOR: u64 = 3;

/// Upper bound on heap held by a waiting connection (socket wrapper, task,
/// timer).
pub const PENDING_OVERHEAD: u64 = 2 << 10;

/// The fixed response written, if it fits without blocking, to a connection
/// refused for lack of a slot. It is never read from or waited on.
pub const BUSY_RESPONSE: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n\
content-type: application/json\r\n\
retry-after: 1\r\n\
connection: close\r\n\
content-length: 86\r\n\
\r\n\
{\"error\":{\"code\":\"server_busy\",\"message\":\"too many open connections; retry shortly\"}}\n\
";

/// Transport limits, taken from `[server]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportLimits {
    pub max_connections: usize,
    pub header_timeout: Duration,
    pub body_timeout: Duration,
    pub write_timeout: Duration,
    pub connection_buffer: usize,
}

impl TransportLimits {
    pub fn from_config(c: &ServerConfig) -> TransportLimits {
        TransportLimits {
            max_connections: c.max_connections,
            header_timeout: c.header_timeout,
            body_timeout: c.body_timeout,
            write_timeout: c.write_timeout,
            connection_buffer: usize::try_from(c.connection_buffer.0).unwrap_or(usize::MAX),
        }
    }

    /// Estimated heap held by one admitted connection whose buffers are
    /// both full.
    pub fn estimated_per_connection(&self) -> u64 {
        (self.connection_buffer as u64).saturating_mul(BUFFER_ALLOCATION_FACTOR).saturating_add(CONNECTION_OVERHEAD)
    }

    /// Estimated heap held by the transport at its limits: every slot with
    /// both buffers full, and every waiting connection. Request and
    /// response bodies are not included: handlers bound those (ingest
    /// admission, query budgets).
    pub fn estimated_peak(&self) -> u64 {
        self.estimated_per_connection()
            .saturating_mul(self.max_connections as u64)
            .saturating_add(PENDING_LIMIT as u64 * PENDING_OVERHEAD)
    }
}

/// How connections ended up, for `/api/v1/system/info`.
#[derive(Debug, Default)]
pub struct TransportStats {
    /// Connections given a slot (at once or after waiting).
    pub admitted: AtomicU64,
    /// Connections that had to wait for a slot.
    pub queued: AtomicU64,
    /// Connections refused: no slot and none freed in time, or too many
    /// waiting.
    pub refused: AtomicU64,
    /// Closed because a request head did not arrive in time (including
    /// idle keep-alive connections).
    pub header_timeouts: AtomicU64,
    /// Request bodies that did not arrive in time.
    pub body_timeouts: AtomicU64,
    /// Closed because the client stopped reading a response.
    pub write_timeouts: AtomicU64,
    /// Ended by an HTTP protocol error (malformed or oversized head,
    /// unsupported protocol).
    pub protocol_errors: AtomicU64,
    /// `accept` failures, such as running out of file descriptors.
    pub accept_errors: AtomicU64,
}

/// Connection admission shared by every listener of one process.
pub struct Transport {
    pub limits: TransportLimits,
    slots: Arc<Semaphore>,
    pending: Arc<Semaphore>,
    pub stats: TransportStats,
}

/// A point-in-time view of [`Transport`], for `/api/v1/system/info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportSnapshot {
    pub max_connections: usize,
    pub active: usize,
    pub pending: usize,
    pub max_pending: usize,
    pub connection_buffer_bytes: usize,
    pub estimated_peak_bytes: u64,
    pub admitted: u64,
    pub queued: u64,
    pub refused: u64,
    pub header_timeouts: u64,
    pub body_timeouts: u64,
    pub write_timeouts: u64,
    pub protocol_errors: u64,
    pub accept_errors: u64,
}

impl Transport {
    pub fn new(limits: TransportLimits) -> Arc<Transport> {
        Arc::new(Transport {
            limits,
            slots: Arc::new(Semaphore::new(limits.max_connections)),
            pending: Arc::new(Semaphore::new(PENDING_LIMIT)),
            stats: TransportStats::default(),
        })
    }

    /// Connections holding a slot.
    pub fn active(&self) -> usize {
        self.limits.max_connections - self.slots.available_permits()
    }

    /// Connections waiting for a slot.
    pub fn pending(&self) -> usize {
        PENDING_LIMIT - self.pending.available_permits()
    }

    pub fn snapshot(&self) -> TransportSnapshot {
        let s = &self.stats;
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        TransportSnapshot {
            max_connections: self.limits.max_connections,
            active: self.active(),
            pending: self.pending(),
            max_pending: PENDING_LIMIT,
            connection_buffer_bytes: self.limits.connection_buffer,
            estimated_peak_bytes: self.limits.estimated_peak(),
            admitted: n(&s.admitted),
            queued: n(&s.queued),
            refused: n(&s.refused),
            header_timeouts: n(&s.header_timeouts),
            body_timeouts: n(&s.body_timeouts),
            write_timeouts: n(&s.write_timeouts),
            protocol_errors: n(&s.protocol_errors),
            accept_errors: n(&s.accept_errors),
        }
    }

    fn count(&self, c: impl Fn(&TransportStats) -> &AtomicU64) {
        c(&self.stats).fetch_add(1, Ordering::Relaxed);
    }
}

/// Serve `router` on `listener` until `stop` resolves, then stop accepting,
/// let every connection finish its current request (idle ones close at
/// once) and resolve when all of them, and every waiting connection, have
/// ended. Live-tail streams and long requests end through the application's
/// own shutdown signal; the caller bounds the wait.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    transport: Arc<Transport>,
    stop: impl Future<Output = ()>,
) -> io::Result<()> {
    // Each connection holds a receiver: `closed()` resolves once all have
    // ended, and a sent `true` asks them to shut down gracefully.
    let (closing, close_rx) = watch::channel(false);
    let mut stop = pin!(stop);
    loop {
        let accepted = tokio::select! {
            r = listener.accept() => r,
            _ = &mut stop => break,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(e) if is_connection_error(&e) => continue,
            Err(e) => {
                transport.count(|s| &s.accept_errors);
                tracing::warn!(error = %e, "cannot accept connections; retrying");
                tokio::select! {
                    _ = tokio::time::sleep(ACCEPT_ERROR_BACKOFF) => continue,
                    _ = &mut stop => break,
                }
            }
        };
        match transport.slots.clone().try_acquire_owned() {
            Ok(slot) => {
                transport.count(|s| &s.admitted);
                tokio::spawn(connection(stream, slot, router.clone(), transport.clone(), close_rx.clone()));
            }
            Err(_) => match transport.pending.clone().try_acquire_owned() {
                Ok(waiting) => {
                    transport.count(|s| &s.queued);
                    tokio::spawn(wait_for_slot(stream, waiting, router.clone(), transport.clone(), close_rx.clone()));
                }
                Err(_) => refuse(&transport, stream),
            },
        }
    }
    drop(listener);
    drop(close_rx);
    let _ = closing.send(true);
    closing.closed().await;
    Ok(())
}

/// Errors that concern one connection, not the listener.
fn is_connection_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset
    )
}

/// Resolves once [`serve`] asks connections to close (or has gone).
async fn closed(closing: &mut watch::Receiver<bool>) {
    let _ = closing.wait_for(|c| *c).await;
}

/// Write the fixed 503 if the socket takes it without blocking, then close.
fn refuse(transport: &Transport, stream: TcpStream) {
    transport.count(|s| &s.refused);
    let _ = stream.try_write(BUSY_RESPONSE);
    drop(stream);
}

async fn wait_for_slot(
    stream: TcpStream,
    waiting: OwnedSemaphorePermit,
    router: Router,
    transport: Arc<Transport>,
    mut closing: watch::Receiver<bool>,
) {
    let slot = tokio::select! {
        r = tokio::time::timeout(PENDING_TIMEOUT, transport.slots.clone().acquire_owned()) => r.ok().and_then(Result::ok),
        _ = closed(&mut closing) => None,
    };
    drop(waiting);
    match slot {
        Some(slot) => {
            transport.count(|s| &s.admitted);
            connection(stream, slot, router, transport, closing).await;
        }
        None => refuse(&transport, stream),
    }
}

async fn connection(
    stream: TcpStream,
    _slot: OwnedSemaphorePermit,
    router: Router,
    transport: Arc<Transport>,
    mut closing: watch::Receiver<bool>,
) {
    let limits = transport.limits;
    let io = TokioIo::new(WriteDeadline::new(stream, transport.clone()));
    let requests = transport.clone();
    let service = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
        let deadline = Instant::now() + requests.limits.body_timeout;
        let req = req.map(|body| Body::new(DeadlineBody::new(body, deadline, requests.clone())));
        tower::ServiceExt::oneshot(router.clone(), req)
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_timeout)
        .max_buf_size(limits.connection_buffer)
        .max_headers(MAX_HEADERS)
        .keep_alive(true);
    let mut conn = pin!(builder.serve_connection(io, service));
    let result = tokio::select! {
        r = conn.as_mut() => r,
        _ = closed(&mut closing) => {
            conn.as_mut().graceful_shutdown();
            conn.await
        }
    };
    if let Err(e) = result {
        if e.is_timeout() {
            transport.count(|s| &s.header_timeouts);
        } else if e.is_parse() || e.is_parse_status() || e.is_parse_too_large() {
            transport.count(|s| &s.protocol_errors);
        }
        tracing::debug!(error = %e, "connection ended with an error");
    }
}

/// A request body that must be complete by `deadline`
/// (`server.body_timeout` after its head arrived). Once it passes, every
/// further read fails with [`BodyTimeout`].
struct DeadlineBody {
    inner: Incoming,
    deadline: Instant,
    /// Created the first time the body has to wait for the client.
    sleep: Option<Pin<Box<Sleep>>>,
    expired: bool,
    transport: Arc<Transport>,
}

impl DeadlineBody {
    fn new(inner: Incoming, deadline: Instant, transport: Arc<Transport>) -> DeadlineBody {
        DeadlineBody { inner, deadline, sleep: None, expired: false, transport }
    }
}

impl http_body::Body for DeadlineBody {
    type Data = Bytes;
    type Error = axum::BoxError;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = &mut *self;
        if this.expired {
            return Poll::Ready(Some(Err(BodyTimeout(this.transport.limits.body_timeout).into())));
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => Poll::Ready(frame.map(|f| f.map_err(Into::into))),
            Poll::Pending => {
                let deadline = this.deadline;
                let sleep = this.sleep.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
                ready!(sleep.as_mut().poll(cx));
                this.expired = true;
                this.transport.count(|s| &s.body_timeouts);
                Poll::Ready(Some(Err(BodyTimeout(this.transport.limits.body_timeout).into())))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        !self.expired && self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// A request body did not arrive within `server.body_timeout`.
#[derive(Debug, Clone, Copy)]
pub struct BodyTimeout(pub Duration);

impl std::fmt::Display for BodyTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "request body not received within {}", crate::config::format_duration(self.0))
    }
}

impl StdError for BodyTimeout {}

/// Whether `e`, or any error it wraps, is a [`BodyTimeout`].
pub fn is_body_timeout(e: &(dyn StdError + 'static)) -> bool {
    let mut next = Some(e);
    while let Some(e) = next {
        if e.is::<BodyTimeout>() {
            return true;
        }
        next = e.source();
    }
    false
}

/// A socket whose writes fail with `TimedOut` once one has been blocked for
/// `server.write_timeout`: the client stopped reading and the kernel's send
/// buffer is full. Any write that makes progress resets the clock, so a
/// slow reader that keeps up is never cut off.
struct WriteDeadline {
    io: TcpStream,
    transport: Arc<Transport>,
    /// Reused across stalls; created the first time a write blocks.
    sleep: Option<Pin<Box<Sleep>>>,
    /// A write is blocked and `sleep` is set to its deadline.
    armed: bool,
}

impl WriteDeadline {
    fn new(io: TcpStream, transport: Arc<Transport>) -> WriteDeadline {
        WriteDeadline { io, transport, sleep: None, armed: false }
    }

    /// The write is blocked: start its clock if this is a new stall, and
    /// fail once it runs out.
    fn blocked<T>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        let timeout = self.transport.limits.write_timeout;
        if !self.armed {
            let at = Instant::now() + timeout;
            match &mut self.sleep {
                Some(s) => s.as_mut().reset(at),
                None => self.sleep = Some(Box::pin(tokio::time::sleep_until(at))),
            }
            self.armed = true;
        }
        let Some(sleep) = self.sleep.as_mut() else { return Poll::Pending };
        ready!(sleep.as_mut().poll(cx));
        self.armed = false;
        self.transport.count(|s| &s.write_timeouts);
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("client stopped reading for {}", crate::config::format_duration(timeout)),
        )))
    }

    fn written<T>(&mut self, r: Poll<io::Result<T>>, cx: &mut Context<'_>) -> Poll<io::Result<T>> {
        match r {
            Poll::Pending => self.blocked(cx),
            done => {
                self.armed = false;
                done
            }
        }
    }
}

impl AsyncRead for WriteDeadline {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for WriteDeadline {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.io).poll_write(cx, buf);
        self.written(r, cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let r = Pin::new(&mut self.io).poll_write_vectored(cx, bufs);
        self.written(r, cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let r = Pin::new(&mut self.io).poll_flush(cx);
        self.written(r, cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let r = Pin::new(&mut self.io).poll_shutdown(cx);
        self.written(r, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_response_is_well_formed() {
        let text = std::str::from_utf8(BUSY_RESPONSE).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let length: usize =
            head.lines().find_map(|l| l.strip_prefix("content-length: ")).and_then(|n| n.parse().ok()).unwrap();
        assert_eq!(body.len(), length);
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["error"]["code"], "server_busy");
        assert!(head.starts_with("HTTP/1.1 503 "));
    }

    #[test]
    fn estimated_peak_counts_slots_buffers_and_waiting() {
        let limits = TransportLimits {
            max_connections: 10,
            header_timeout: Duration::from_secs(1),
            body_timeout: Duration::from_secs(1),
            write_timeout: Duration::from_secs(1),
            connection_buffer: 16 << 10,
        };
        assert_eq!(
            limits.estimated_peak(),
            10 * (BUFFER_ALLOCATION_FACTOR * (16 << 10) + CONNECTION_OVERHEAD)
                + PENDING_LIMIT as u64 * PENDING_OVERHEAD
        );
        let huge = TransportLimits { max_connections: usize::MAX, connection_buffer: usize::MAX, ..limits };
        assert_eq!(huge.estimated_peak(), u64::MAX, "saturates");
    }

    #[test]
    fn body_timeout_is_found_through_wrappers() {
        let e = io::Error::other(axum::Error::new(BodyTimeout(Duration::from_secs(3))));
        assert!(is_body_timeout(&e));
        assert!(!is_body_timeout(&io::Error::other("other")));
        assert_eq!(BodyTimeout(Duration::from_secs(3)).to_string(), "request body not received within 3s");
    }
}
