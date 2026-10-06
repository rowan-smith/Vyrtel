use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use event::{Event, EventType};
use query::{parse_query, plan_filter, ComparisonOperator, Expression, QueryValue};
use serde::{Deserialize, Serialize};
use storage::EventSearchParams;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/traces", get(list_traces))
        .route("/api/traces/{trace_id}", get(get_trace))
}

#[derive(Debug, Deserialize)]
struct ListTracesParams {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: Option<usize>,
    q: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceSummary {
    trace_id: String,
    timestamp: DateTime<Utc>,
    root_operation: Option<String>,
    service: Option<String>,
    duration_ns: Option<u64>,
    status: String,
    span_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListTracesResponse {
    traces: Vec<TraceSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TraceDetail {
    trace_id: String,
    spans: Vec<Event>,
    logs: Vec<Event>,
}

async fn list_traces(
    State(state): State<AppState>,
    Query(params): Query<ListTracesParams>,
) -> Result<Json<ListTracesResponse>, ApiError> {
    let mut filter = Expression::Comparison {
        field: "event_type".into(),
        operator: ComparisonOperator::Eq,
        value: QueryValue::String("span".into()),
    };

    if let Some(q) = params.q.as_deref().filter(|s| !s.trim().is_empty()) {
        let parsed = parse_query(q)?;
        if let Some(user_filter) = parsed.filter {
            filter = Expression::And(Box::new(filter), Box::new(user_filter));
        }
    }

    let tantivy_query = plan_filter(state.store.index(), Some(&filter))?;
    let limit = params.limit.unwrap_or(100).clamp(1, 500);

    let result = state
        .store
        .search(EventSearchParams {
            query: tantivy_query,
            from: params.from,
            to: params.to,
            limit: limit.saturating_mul(20), // fetch extra to group
            cursor: None,
        })
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "search_failed",
                e.to_string(),
            )
        })?;

    let mut by_trace: std::collections::BTreeMap<String, Vec<Event>> =
        std::collections::BTreeMap::new();
    for event in result.events {
        if let Some(tid) = event.trace_id.clone() {
            by_trace.entry(tid).or_default().push(event);
        }
    }

    let mut traces: Vec<TraceSummary> = by_trace
        .into_iter()
        .map(|(trace_id, spans)| summarize_trace(trace_id, spans))
        .collect();

    traces.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    traces.truncate(limit);

    Ok(Json(ListTracesResponse { traces }))
}

fn summarize_trace(trace_id: String, mut spans: Vec<Event>) -> TraceSummary {
    spans.sort_by_key(|s| s.timestamp);
    let root = spans
        .iter()
        .find(|s| {
            s.parent_span_id
                .as_ref()
                .map(|p| p.is_empty())
                .unwrap_or(true)
        })
        .or_else(|| spans.first());

    let duration_ns = root
        .and_then(|r| r.duration_ns)
        .or_else(|| spans.iter().filter_map(|s| s.duration_ns).max());

    let status = spans
        .iter()
        .find_map(|s| {
            s.attributes
                .get("otel.status_code")
                .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| Some(v.to_string())))
        })
        .unwrap_or_else(|| "unset".into());

    TraceSummary {
        timestamp: root.map(|r| r.timestamp).unwrap_or_else(Utc::now),
        root_operation: root.and_then(|r| r.message.clone()),
        service: root.and_then(|r| r.service.clone()),
        duration_ns,
        status,
        span_count: spans.len(),
        trace_id,
    }
}

async fn get_trace(
    State(state): State<AppState>,
    Path(trace_id): Path<String>,
) -> Result<Json<TraceDetail>, ApiError> {
    let filter = Expression::Comparison {
        field: "trace_id".into(),
        operator: ComparisonOperator::Eq,
        value: QueryValue::String(trace_id.clone()),
    };
    let tantivy_query = plan_filter(state.store.index(), Some(&filter))?;
    let result = state
        .store
        .search(EventSearchParams {
            query: tantivy_query,
            from: None,
            to: None,
            limit: 5000,
            cursor: None,
        })
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "search_failed",
                e.to_string(),
            )
        })?;

    let mut spans = Vec::new();
    let mut logs = Vec::new();
    for event in result.events {
        match event.event_type {
            EventType::Span => spans.push(event),
            EventType::Log | EventType::Metric => logs.push(event),
        }
    }
    spans.sort_by_key(|s| s.timestamp);
    logs.sort_by_key(|s| s.timestamp);

    if spans.is_empty() && logs.is_empty() {
        return Err(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("Trace '{trace_id}' not found"),
        ));
    }

    Ok(Json(TraceDetail {
        trace_id,
        spans,
        logs,
    }))
}
