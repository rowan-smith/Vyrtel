use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use otlp::{convert_logs, convert_traces, ExportLogsServiceRequest, ExportTraceServiceRequest};
use serde_json::json;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/logs", post(otlp_logs))
        .route("/v1/traces", post(otlp_traces))
}

async fn otlp_logs(
    State(state): State<AppState>,
    Json(payload): Json<ExportLogsServiceRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let events = convert_logs(payload);
    let count = events.len();
    state
        .ingest
        .try_enqueue_batch(events)
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "queue_full",
                "Ingestion queue is full",
            )
        })?;
    Ok((StatusCode::OK, Json(json!({ "partialSuccess": {}, "accepted": count }))))
}

async fn otlp_traces(
    State(state): State<AppState>,
    Json(payload): Json<ExportTraceServiceRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let events = convert_traces(payload);
    let count = events.len();
    state
        .ingest
        .try_enqueue_batch(events)
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "queue_full",
                "Ingestion queue is full",
            )
        })?;
    Ok((StatusCode::OK, Json(json!({ "partialSuccess": {}, "accepted": count }))))
}
