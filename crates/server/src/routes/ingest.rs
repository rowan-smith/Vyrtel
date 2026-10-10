//! Ingestion endpoints: native JSON/NDJSON and OTLP/HTTP.
//!
//! Path: admission permit → HTTP body (size-limited, gzip-decoded) → parse
//! on the blocking pool → bounded storage queue → WAL (+fsync in strict
//! mode) → response. The permit is held for that whole lifecycle, so
//! `ingest.max_concurrent` bounds bodies, decoded events and requests
//! waiting for their acknowledgement alike. With no permit free the request
//! gets 429 with `Retry-After` before its body is read, and the body is
//! then discarded (never buffered) so the client sees the 429 rather than a
//! reset connection. A full queue also gets 429. The server never buffers
//! unboundedly on behalf of a client.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use ingest::{IngestError, native, otlp};
use serde_json::json;
use storage::Committed;
use telemetry::{Signal, TelemetryEvent, Timestamp};
use tokio::sync::{OwnedSemaphorePermit, oneshot};

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SharedState};

fn content_type(h: &HeaderMap) -> Option<&str> {
    h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
}

/// One admitted ingest request. Dropping every clone frees the slot, so a
/// request that finishes, fails or is cancelled (client disconnect) gives
/// its slot back. A clone travels into the blocking parse task: a request
/// cancelled mid-parse keeps its slot until the parse, and the memory it
/// holds, is gone.
#[derive(Clone)]
pub struct IngestPermit {
    _slot: Arc<OwnedSemaphorePermit>,
}

/// Rejected request bodies being read and dropped at once. Others wait
/// unread (the client is held back by TCP flow control, nothing is
/// buffered) until a slot frees or [`DISCARD_TIMEOUT`] passes.
pub const MAX_DISCARDING: usize = 32;
/// Rejected bodies kept at all, reading or waiting: a hard cap on discard
/// tasks. Beyond it a rejected body is dropped at once, so the client may
/// see a reset connection instead of the 429.
pub const MAX_DISCARD_PENDING: usize = 256;
/// Give up on a rejected body after this long and close the connection.
const DISCARD_TIMEOUT: Duration = Duration::from_secs(10);

/// Take an ingest slot without waiting. When none is free, fail with 429
/// before any of the body is read. `body` is handed back on success; on
/// rejection it is discarded in the background.
pub fn admit(state: &AppState, body: Body) -> ApiResult<(IngestPermit, Body)> {
    match state.ingest_permits.clone().try_acquire_owned() {
        Ok(p) => Ok((IngestPermit { _slot: Arc::new(p) }, body)),
        Err(_) => {
            state.ingest_rejected.fetch_add(1, Ordering::Relaxed);
            discard(state, body);
            Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many concurrent ingest requests; retry shortly",
            ))
        }
    }
}

/// Read and drop a rejected body, one chunk at a time, while the response
/// goes out. Closing the connection with the upload unread makes many
/// clients report a reset instead of the response. Bounded in bytes
/// (`ingest.max_request_size`, after gzip decoding), time, concurrency and
/// number of tasks; past any bound the body is dropped and the connection
/// closes.
fn discard(state: &AppState, body: Body) {
    let Ok(pending) = state.discard_pending.clone().try_acquire_owned() else { return };
    let slots = state.discard_permits.clone();
    let limit = state.config.ingest.max_request_size.0 as usize;
    tokio::spawn(tokio::time::timeout(DISCARD_TIMEOUT, async move {
        let _pending = pending;
        let Ok(_slot) = slots.acquire_owned().await else { return };
        let mut data = body.into_data_stream();
        let mut read = 0;
        while let Some(Ok(chunk)) = data.next().await {
            read += chunk.len();
            if read > limit {
                break;
            }
        }
    }));
}

/// Read a request body within `ingest.max_request_size`. The limit applies
/// after gzip decoding, so compressed "bombs" are cut off too.
pub async fn read_body(state: &AppState, _permit: &IngestPermit, body: Body) -> ApiResult<Bytes> {
    let limit = state.config.ingest.max_request_size.0 as usize;
    match axum::body::to_bytes(body, limit).await {
        Ok(b) => {
            state.received_bytes.fetch_add(b.len() as u64, Ordering::Relaxed);
            state.received_requests.fetch_add(1, Ordering::Relaxed);
            Ok(b)
        }
        Err(e) => {
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&e);
            while let Some(err) = source {
                if err.is::<http_body_util::LengthLimitError>() {
                    return Err(ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "payload_too_large",
                        format!("request body exceeds the {} limit", state.config.ingest.max_request_size),
                    ));
                }
                source = err.source();
            }
            Err(ApiError::bad_request(format!("could not read request body: {e}")))
        }
    }
}

/// Parse on the blocking pool while holding `permit`.
async fn parse<T, F>(permit: &IngestPermit, f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, IngestError> + Send + 'static,
{
    let held = permit.clone();
    let r = tokio::task::spawn_blocking(move || {
        let _held = held;
        f()
    })
    .await??;
    Ok(r)
}

/// Queue events and wait until they are durable and queryable.
pub async fn submit(state: &AppState, signal: Signal, events: Vec<TelemetryEvent>) -> ApiResult<Committed> {
    let (tx, rx) = oneshot::channel();
    state.storage.submit(
        signal,
        events,
        Box::new(move |r| {
            let _ = tx.send(r);
        }),
    )?;
    match tokio::time::timeout(state.config.ingest.ack_timeout, rx).await {
        Ok(Ok(Ok(c))) => Ok(c),
        Ok(Ok(Err(e))) => Err(e.into()),
        Ok(Err(_)) => Err(ApiError::unavailable("storage stopped before the write completed")),
        Err(_) => Err(ApiError::unavailable("timed out waiting for the write to complete")),
    }
}

pub async fn native_events(State(state): State<SharedState>, headers: HeaderMap, body: Body) -> ApiResult<Response> {
    let format = native::Format::from_content_type(content_type(&headers))?;
    let (permit, body) = admit(&state, body)?;
    let bytes = read_body(&state, &permit, body).await?;
    let now = Timestamp::now();
    let events = parse(&permit, move || native::parse(&bytes, format, now)).await?;
    let n = events.len();
    submit(&state, Signal::Logs, events).await?;
    Ok((StatusCode::OK, Json(json!({ "accepted": n }))).into_response())
}

async fn otlp_export(
    state: SharedState,
    headers: HeaderMap,
    body: Body,
    signal: Signal,
    decode: fn(&[u8], otlp::Encoding, Timestamp) -> Result<otlp::Mapped, IngestError>,
) -> ApiResult<Response> {
    let enc = otlp::Encoding::from_content_type(content_type(&headers))?;
    let (permit, body) = admit(&state, body)?;
    let bytes = read_body(&state, &permit, body).await?;
    let now = Timestamp::now();
    let mapped = parse(&permit, move || decode(&bytes, enc, now)).await?;
    if !mapped.events.is_empty() {
        submit(&state, signal, mapped.events).await?;
    }
    let body = otlp::encode_response(enc, mapped.rejected, mapped.error.as_deref());
    Ok(([(header::CONTENT_TYPE, enc.content_type())], body).into_response())
}

pub async fn otlp_logs(State(state): State<SharedState>, headers: HeaderMap, body: Body) -> ApiResult<Response> {
    otlp_export(state, headers, body, Signal::Logs, otlp::decode_logs).await
}

pub async fn otlp_traces(State(state): State<SharedState>, headers: HeaderMap, body: Body) -> ApiResult<Response> {
    otlp_export(state, headers, body, Signal::Traces, otlp::decode_traces).await
}

pub async fn otlp_metrics(State(state): State<SharedState>, headers: HeaderMap, body: Body) -> ApiResult<Response> {
    otlp_export(state, headers, body, Signal::Metrics, otlp::decode_metrics).await
}
