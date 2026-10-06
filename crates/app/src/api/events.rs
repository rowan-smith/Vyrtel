use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use event::{Event, IngestEventRequest};
use futures::stream::Stream;
use ingest::IngestError;
use serde::Serialize;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn ingest_router() -> Router<AppState> {
    Router::new()
        .route("/api/events", post(ingest_one))
        .route("/api/events/bulk", post(ingest_bulk))
}

pub fn query_router() -> Router<AppState> {
    Router::new()
        .route("/api/events", get(super::query_api::list_events))
        .route("/api/events/stream", get(stream_events))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestResponse {
    id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BulkIngestResponse {
    accepted: usize,
}

async fn ingest_one(
    State(state): State<AppState>,
    Json(payload): Json<IngestEventRequest>,
) -> Result<(StatusCode, Json<IngestResponse>), ApiError> {
    let event = payload.into_event();
    let id = event.id.to_string();
    enqueue(&state, event)?;
    Ok((StatusCode::ACCEPTED, Json(IngestResponse { id })))
}

async fn ingest_bulk(
    State(state): State<AppState>,
    Json(payload): Json<Vec<IngestEventRequest>>,
) -> Result<(StatusCode, Json<BulkIngestResponse>), ApiError> {
    let events: Vec<Event> = payload.into_iter().map(|p| p.into_event()).collect();
    let accepted = events.len();
    state
        .ingest
        .try_enqueue_batch(events)
        .map_err(map_ingest_error)?;
    Ok((StatusCode::ACCEPTED, Json(BulkIngestResponse { accepted })))
}

fn enqueue(state: &AppState, event: Event) -> Result<(), ApiError> {
    state.ingest.try_enqueue(event).map_err(map_ingest_error)
}

fn map_ingest_error(err: IngestError) -> ApiError {
    match err {
        IngestError::QueueFull => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "queue_full", "Ingestion queue is full")
        }
    }
}

async fn stream_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = state.ingest.subscribe_live();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let json = serde_json::to_string(&event).ok()?;
            Some(Ok(SseEvent::default().event("event").data(json)))
        }
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
