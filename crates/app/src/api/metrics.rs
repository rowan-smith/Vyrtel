use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use event::{Event, EventType};
use query::{plan_filter, ComparisonOperator, Expression, QueryValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use storage::EventSearchParams;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn ingest_router() -> Router<AppState> {
    Router::new()
        .route("/api/metrics", post(ingest_metric))
        .route("/api/metrics/bulk", post(ingest_metrics_bulk))
}

pub fn query_router() -> Router<AppState> {
    Router::new()
        .route("/api/metrics", get(list_metrics))
        .route("/api/metrics/series", get(metric_series))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestMetricRequest {
    pub name: String,
    pub value: f64,
    pub timestamp: Option<DateTime<Utc>>,
    pub unit: Option<String>,
    pub service: Option<String>,
    pub environment: Option<String>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
}

impl IngestMetricRequest {
    pub fn into_event(self) -> Event {
        let mut attributes = self.attributes;
        attributes.insert("value".into(), json!(self.value));
        if let Some(unit) = self.unit {
            attributes.insert("unit".into(), Value::String(unit));
        }
        Event {
            id: uuid::Uuid::new_v4(),
            timestamp: self.timestamp.unwrap_or_else(Utc::now),
            event_type: EventType::Metric,
            level: None,
            message: Some(self.name),
            message_template: None,
            service: self.service,
            environment: self.environment,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
            duration_ns: None,
            stacktrace: None,
            attributes,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IngestResponse {
    id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BulkResponse {
    accepted: usize,
}

async fn ingest_metric(
    State(state): State<AppState>,
    Json(body): Json<IngestMetricRequest>,
) -> Result<(StatusCode, Json<IngestResponse>), ApiError> {
    if body.name.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Metric name is required",
        ));
    }
    let event = body.into_event();
    let id = event.id.to_string();
    state.ingest.try_enqueue(event).map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "queue_full",
            "Ingestion queue is full",
        )
    })?;
    Ok((StatusCode::ACCEPTED, Json(IngestResponse { id })))
}

async fn ingest_metrics_bulk(
    State(state): State<AppState>,
    Json(body): Json<Vec<IngestMetricRequest>>,
) -> Result<(StatusCode, Json<BulkResponse>), ApiError> {
    let events: Vec<Event> = body.into_iter().map(|m| m.into_event()).collect();
    let accepted = events.len();
    state.ingest.try_enqueue_batch(events).map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "queue_full",
            "Ingestion queue is full",
        )
    })?;
    Ok((StatusCode::ACCEPTED, Json(BulkResponse { accepted })))
}

#[derive(Deserialize)]
struct ListMetricsParams {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MetricSummary {
    name: String,
    service: Option<String>,
    unit: Option<String>,
    last_value: Option<f64>,
    last_timestamp: Option<DateTime<Utc>>,
    point_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListMetricsResponse {
    metrics: Vec<MetricSummary>,
}

async fn list_metrics(
    State(state): State<AppState>,
    Query(params): Query<ListMetricsParams>,
) -> Result<Json<ListMetricsResponse>, ApiError> {
    let filter = Expression::Comparison {
        field: "event_type".into(),
        operator: ComparisonOperator::Eq,
        value: QueryValue::String("metric".into()),
    };
    let query = plan_filter(state.store.index(), Some(&filter))?;
    let result = state
        .store
        .search(EventSearchParams {
            query,
            from: params.from,
            to: params.to,
            limit: params.limit.unwrap_or(2000).clamp(1, 5000),
            cursor: None,
        })
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "search_failed",
                e.to_string(),
            )
        })?;

    let mut by_name: std::collections::BTreeMap<String, MetricSummary> =
        std::collections::BTreeMap::new();
    for event in result.events {
        let name = event.message.unwrap_or_else(|| "(unnamed)".into());
        let value = event
            .attributes
            .get("value")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                event
                    .attributes
                    .get("value")
                    .and_then(|v| v.as_i64().map(|i| i as f64))
            });
        let unit = event
            .attributes
            .get("unit")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let entry = by_name.entry(name.clone()).or_insert(MetricSummary {
            name,
            service: event.service.clone(),
            unit: unit.clone(),
            last_value: value,
            last_timestamp: Some(event.timestamp),
            point_count: 0,
        });
        entry.point_count += 1;
        // Events are newest-first; keep first seen as last_*
        if entry.last_timestamp == Some(event.timestamp) || entry.point_count == 1 {
            entry.last_value = value;
            entry.last_timestamp = Some(event.timestamp);
            if entry.service.is_none() {
                entry.service = event.service;
            }
            if entry.unit.is_none() {
                entry.unit = unit;
            }
        }
    }

    Ok(Json(ListMetricsResponse {
        metrics: by_name.into_values().collect(),
    }))
}

#[derive(Deserialize)]
struct SeriesParams {
    name: String,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    service: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SeriesPoint {
    timestamp: DateTime<Utc>,
    value: f64,
    service: Option<String>,
    attributes: Map<String, Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SeriesResponse {
    name: String,
    points: Vec<SeriesPoint>,
}

async fn metric_series(
    State(state): State<AppState>,
    Query(params): Query<SeriesParams>,
) -> Result<Json<SeriesResponse>, ApiError> {
    let filter = Expression::Comparison {
        field: "event_type".into(),
        operator: ComparisonOperator::Eq,
        value: QueryValue::String("metric".into()),
    };
    let query = plan_filter(state.store.index(), Some(&filter))?;
    let result = state
        .store
        .search(EventSearchParams {
            query,
            from: params.from,
            to: params.to,
            limit: 5000,
            cursor: None,
        })
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "search_failed",
                e.to_string(),
            )
        })?;

    let limit = params.limit.unwrap_or(500).clamp(1, 5000);
    let mut points: Vec<SeriesPoint> = result
        .events
        .into_iter()
        .filter(|event| event.message.as_deref() == Some(params.name.as_str()))
        .filter(|event| {
            params
                .service
                .as_ref()
                .map(|s| event.service.as_deref() == Some(s.as_str()))
                .unwrap_or(true)
        })
        .filter_map(|event| {
            let value = event.attributes.get("value").and_then(|v| {
                v.as_f64()
                    .or_else(|| v.as_i64().map(|i| i as f64))
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })?;
            Some(SeriesPoint {
                timestamp: event.timestamp,
                value,
                service: event.service,
                attributes: event.attributes,
            })
        })
        .take(limit)
        .collect();
    points.sort_by_key(|p| p.timestamp);

    Ok(Json(SeriesResponse {
        name: params.name,
        points,
    }))
}
