//! Ingestion endpoints: native JSON/NDJSON and OTLP/HTTP.
//!
//! Path: HTTP body (size-limited, gzip-decoded) → parse on the blocking
//! pool → bounded storage queue → WAL (+fsync in strict mode) → response.
//! A full queue returns 429 with `Retry-After`; the server never buffers
//! unboundedly on behalf of a client.

use std::sync::atomic::Ordering;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use ingest::{native, otlp};
use serde_json::json;
use storage::Committed;
use telemetry::{Signal, TelemetryEvent, Timestamp};
use tokio::sync::oneshot;

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SharedState};

fn content_type(h: &HeaderMap) -> Option<&str> {
    h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
}

/// Read a request body within `ingest.max_request_size`. The limit applies
/// after gzip decoding, so compressed "bombs" are cut off too.
pub async fn read_body(state: &AppState, body: Body) -> ApiResult<Bytes> {
    let _permit = state.ingest_permits.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "too many concurrent ingest requests")
    })?;
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
    let bytes = read_body(&state, body).await?;
    let now = Timestamp::now();
    let events = tokio::task::spawn_blocking(move || native::parse(&bytes, format, now)).await??;
    let n = events.len();
    submit(&state, Signal::Logs, events).await?;
    Ok((StatusCode::OK, Json(json!({ "accepted": n }))).into_response())
}

async fn otlp_export(
    state: SharedState,
    headers: HeaderMap,
    body: Body,
    signal: Signal,
    decode: fn(&[u8], otlp::Encoding, Timestamp) -> Result<otlp::Mapped, ingest::IngestError>,
) -> ApiResult<Response> {
    let enc = otlp::Encoding::from_content_type(content_type(&headers))?;
    let bytes = read_body(&state, body).await?;
    let now = Timestamp::now();
    let mapped = tokio::task::spawn_blocking(move || decode(&bytes, enc, now)).await??;
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
