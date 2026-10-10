//! Ingestion endpoints: native JSON/NDJSON and OTLP/HTTP.
//!
//! Path: admission permit → HTTP body (gzip-decoded, size-limited before
//! and after decoding) → parse on the blocking pool → bounded storage
//! queue → WAL (+fsync in strict mode) → response. The permit is held for
//! that whole lifecycle, so `ingest.max_concurrent` bounds bodies, decoded
//! events and requests waiting for their acknowledgement alike. With no
//! permit free the request gets 429 with `Retry-After` before its body is
//! read, and the body is then discarded (never buffered) so the client sees
//! the 429 rather than a reset connection. A full queue also gets 429. The
//! server never buffers unboundedly on behalf of a client.

use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_compression::tokio::bufread::GzipDecoder;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::{Stream, StreamExt};
use ingest::{IngestError, native, otlp};
use serde_json::json;
use storage::Committed;
use telemetry::{Signal, TelemetryEvent, Timestamp};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{OwnedSemaphorePermit, oneshot};
use tokio_util::io::StreamReader;

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SharedState};

fn content_type(h: &HeaderMap) -> Option<&str> {
    h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
}

/// How an ingest request body is encoded on the wire (`Content-Encoding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyEncoding {
    Identity,
    Gzip,
}

impl BodyEncoding {
    /// Checked before admission, like the content type: anything but gzip
    /// or identity gets a 415 naming the encodings that are accepted.
    pub fn from_headers(h: &HeaderMap) -> ApiResult<Self> {
        let mut values = h.get_all(header::CONTENT_ENCODING).iter();
        let v = match (values.next(), values.next()) {
            (None, _) => return Ok(Self::Identity),
            (Some(v), None) => v.to_str().unwrap_or("(not ASCII)").trim(),
            (Some(_), Some(_)) => return Err(unsupported_encoding("more than one Content-Encoding header")),
        };
        if v.is_empty() || v.eq_ignore_ascii_case("identity") {
            Ok(Self::Identity)
        } else if v.eq_ignore_ascii_case("gzip") || v.eq_ignore_ascii_case("x-gzip") {
            Ok(Self::Gzip)
        } else {
            Err(unsupported_encoding(&format!("Content-Encoding {v:?} is not supported")))
        }
    }
}

fn unsupported_encoding(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        format!("{what}; send the body uncompressed or with Content-Encoding: gzip"),
    )
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
            discard(state, body.into_data_stream());
            Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many concurrent ingest requests; retry shortly",
            ))
        }
    }
}

/// Read and drop the unread rest of a rejected body (all of it after a
/// 429; what follows the point of failure after a 413 or a bad gzip
/// stream), one chunk at a time, while the response goes out. Closing the
/// connection with the upload unread makes many clients report a reset
/// instead of the response. Bounded in bytes (`ingest.max_request_size` of
/// wire bytes), time, concurrency and number of tasks; past any bound the
/// body is dropped and the connection closes.
fn discard<S, E>(state: &AppState, mut data: S)
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
{
    let Ok(pending) = state.discard_pending.clone().try_acquire_owned() else { return };
    let slots = state.discard_permits.clone();
    let limit = state.config.ingest.max_request_size.0 as usize;
    tokio::spawn(tokio::time::timeout(DISCARD_TIMEOUT, async move {
        let _pending = pending;
        let Ok(_slot) = slots.acquire_owned().await else { return };
        let mut read = 0;
        while let Some(Ok(chunk)) = data.next().await {
            read += chunk.len();
            if read > limit {
                break;
            }
        }
    }));
}

/// Decoded bytes requested from the body per read. Bounds the work done
/// past the limit before an oversized body (e.g. a gzip bomb) is cut off.
const READ_CHUNK: usize = 64 << 10;

enum ReadError {
    /// More than `ingest.max_request_size` bytes after decoding.
    TooLarge,
    /// The gzip stream is truncated, corrupt or followed by other data.
    Decode(io::Error),
    /// The body itself could not be read (e.g. the client went away).
    Transport(io::Error),
}

/// The body passed `ingest.max_request_size` on the wire.
#[derive(Debug)]
struct WireLimitExceeded;

impl std::fmt::Display for WireLimitExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("request body exceeds the size limit")
    }
}

impl std::error::Error for WireLimitExceeded {}

/// Read a request body within `ingest.max_request_size`, decoding gzip as
/// it arrives. The limit applies to the decoded bytes, so decoding stops as
/// soon as a compressed "bomb" passes it, and also to the bytes on the wire,
/// so a stream of empty gzip members cannot be read forever. Concatenated
/// gzip members are all decoded; anything after the last one is an error,
/// never ignored. An uncompressed body whose `Content-Length` is over the
/// limit is rejected before any of it is read. After any failure the unread
/// rest of the body is discarded so the client receives the error.
pub async fn read_body(
    state: &AppState,
    _permit: &IngestPermit,
    encoding: BodyEncoding,
    headers: &HeaderMap,
    body: Body,
) -> ApiResult<Bytes> {
    let limit = state.config.ingest.max_request_size.0 as usize;
    let declared = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
    // Fails once, on the chunk that crosses the limit; later chunks pass so
    // `discard` can still drain them.
    let mut wire = 0usize;
    let data = body.into_data_stream().map(move |chunk| {
        let chunk = chunk.map_err(io::Error::other)?;
        let before = wire;
        wire = wire.saturating_add(chunk.len());
        if before <= limit && wire > limit {
            return Err(io::Error::other(WireLimitExceeded));
        }
        Ok(chunk)
    });
    let mut reader = StreamReader::new(data);
    let read = match encoding {
        BodyEncoding::Identity if declared.is_some_and(|n| n > limit as u64) => {
            discard(state, reader.into_inner());
            Err(ReadError::TooLarge)
        }
        BodyEncoding::Identity => {
            // An honest Content-Length lets the body land in one buffer.
            let hint = declared.map_or(READ_CHUNK, |n| n as usize);
            let read = read_limited(&mut reader, limit, hint).await;
            if read.is_err() {
                discard(state, reader.into_inner());
            }
            read
        }
        BodyEncoding::Gzip => {
            let mut decoder = GzipDecoder::new(reader);
            decoder.multiple_members(true);
            let read = read_limited(&mut decoder, limit, READ_CHUNK).await;
            if read.is_err() {
                discard(state, decoder.into_inner().into_inner());
            }
            read
        }
    };
    let err = match read {
        Ok(b) => {
            state.received_bytes.fetch_add(b.len() as u64, Ordering::Relaxed);
            state.received_requests.fetch_add(1, Ordering::Relaxed);
            return Ok(b);
        }
        Err(ReadError::TooLarge) => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!(
                "request body exceeds the {} limit (measured after gzip decoding); split the batch into smaller requests",
                state.config.ingest.max_request_size
            ),
        )
        .with_details(json!({ "limitBytes": limit })),
        Err(ReadError::Decode(e)) => ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_encoding",
            format!(
                "gzip body could not be decoded ({e}); send complete gzip data: one or more members and nothing after the last"
            ),
        ),
        Err(ReadError::Transport(e)) => ApiError::bad_request(format!("could not read request body: {e}")),
    };
    // The rest of the body is being discarded in the background and may not
    // be read to the end; the connection must not carry another request.
    Err(err.closing_connection())
}

/// Errors from the body stream pass through the gzip decoder unchanged; any
/// other error is the decoder's.
fn read_error(e: io::Error) -> ReadError {
    match e.get_ref() {
        Some(inner) if inner.is::<WireLimitExceeded>() => ReadError::TooLarge,
        Some(inner) if inner.is::<axum::Error>() => ReadError::Transport(e),
        _ => ReadError::Decode(e),
    }
}

/// Read `r` to the end, failing as soon as more than `limit` bytes have
/// come out. The first buffer holds `hint` bytes, later ones
/// [`READ_CHUNK`]; they are joined at the end, so a body is held at most
/// twice (as with `axum::body::to_bytes`), and once when `hint` was right.
async fn read_limited<R: AsyncRead + Unpin>(r: &mut R, limit: usize, hint: usize) -> Result<Bytes, ReadError> {
    let mut parts: Vec<Bytes> = Vec::new();
    let mut buf = Vec::with_capacity(hint.clamp(1, limit.saturating_add(1)));
    let mut total = 0usize;
    loop {
        if buf.len() == buf.capacity() {
            parts.push(Bytes::from(std::mem::replace(&mut buf, Vec::with_capacity(READ_CHUNK))));
        }
        let n = r.read_buf(&mut buf).await.map_err(read_error)?;
        if n == 0 {
            break;
        }
        total += n;
        if total > limit {
            return Err(ReadError::TooLarge);
        }
    }
    if !buf.is_empty() {
        parts.push(Bytes::from(buf));
    }
    Ok(match parts.len() {
        0 => Bytes::new(),
        1 => parts.swap_remove(0),
        _ => {
            let mut joined = Vec::with_capacity(total);
            for part in parts {
                joined.extend_from_slice(&part);
            }
            Bytes::from(joined)
        }
    })
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
    let encoding = BodyEncoding::from_headers(&headers)?;
    let (permit, body) = admit(&state, body)?;
    let bytes = read_body(&state, &permit, encoding, &headers, body).await?;
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
    let encoding = BodyEncoding::from_headers(&headers)?;
    let (permit, body) = admit(&state, body)?;
    let bytes = read_body(&state, &permit, encoding, &headers, body).await?;
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(data).unwrap();
        gz.finish().unwrap()
    }

    // A bomb is cut off once the limit is passed: only a small part of the
    // compressed input is ever decoded.
    #[tokio::test]
    async fn decoding_stops_at_the_limit() {
        let bomb = gzip(&vec![0u8; 256 << 20]);
        let mut decoder = GzipDecoder::new(&bomb[..]);
        decoder.multiple_members(true);
        let r = read_limited(&mut decoder, 64 << 10, READ_CHUNK).await;
        assert!(matches!(r, Err(ReadError::TooLarge)));
        let consumed = bomb.len() - decoder.into_inner().len();
        assert!(consumed < bomb.len() / 100, "decoded {consumed} of {} compressed bytes", bomb.len());
    }

    #[tokio::test]
    async fn limit_is_inclusive_and_parts_are_joined() {
        let data: Vec<u8> = (0..(200 << 10)).map(|i| (i % 251) as u8).collect();
        for (len, hint) in [(0, READ_CHUNK), (1, 1), (data.len(), 7), (data.len(), data.len())] {
            let got = read_limited(&mut &data[..len], data.len(), hint).await.ok().unwrap();
            assert_eq!(&got[..], &data[..len]);
        }
        assert!(matches!(read_limited(&mut &data[..], data.len() - 1, READ_CHUNK).await, Err(ReadError::TooLarge)));
    }

    #[test]
    fn content_encoding() {
        let enc = |values: &[&str]| {
            let mut h = HeaderMap::new();
            for v in values {
                h.append(header::CONTENT_ENCODING, v.parse().unwrap());
            }
            BodyEncoding::from_headers(&h).map_err(|e| (e.status, e.message))
        };
        assert_eq!(enc(&[]), Ok(BodyEncoding::Identity));
        assert_eq!(enc(&["identity"]), Ok(BodyEncoding::Identity));
        assert_eq!(enc(&["GZIP"]), Ok(BodyEncoding::Gzip));
        assert_eq!(enc(&["x-gzip"]), Ok(BodyEncoding::Gzip));
        for bad in [&["br"][..], &["deflate"], &["gzip, gzip"], &["gzip", "gzip"]] {
            let (status, message) = enc(bad).unwrap_err();
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{bad:?}");
            assert!(message.contains("Content-Encoding: gzip"), "{message}");
        }
    }
}
