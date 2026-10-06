use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use event::Event;
use query::{execute_aggregation, parse_query, plan_filter, AggregationResult};
use serde::{Deserialize, Serialize};
use storage::{EventSearchParams, SearchCursor};

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/query", get(run_query))
}

#[derive(Debug, Deserialize)]
pub struct ListEventsParams {
    pub q: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEventsResponse {
    pub events: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

pub async fn list_events(
    State(state): State<AppState>,
    Query(params): Query<ListEventsParams>,
) -> Result<Json<ListEventsResponse>, ApiError> {
    let q = params.q.unwrap_or_default();
    let parsed = parse_query(&q)?;
    if parsed.aggregation.is_some() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_query",
            "Aggregations are not supported on /api/events; use /api/query",
        ));
    }

    let tantivy_query = plan_filter(state.store.index(), parsed.filter.as_ref())?;
    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let cursor = match params.cursor.as_deref() {
        Some(c) => Some(SearchCursor::decode(c).map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "Invalid pagination cursor",
            )
        })?),
        None => None,
    };

    let result = state
        .store
        .search(EventSearchParams {
            query: tantivy_query,
            from: params.from,
            to: params.to,
            limit,
            cursor,
        })
        .map_err(|e| ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, "search_failed", e.to_string()))?;

    Ok(Json(ListEventsResponse {
        events: result.events,
        next_cursor: result.next_cursor,
    }))
}

#[derive(Debug, Deserialize)]
struct QueryParams {
    q: Option<String>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
enum QueryResponse {
    Events(ListEventsResponse),
    Aggregation {
        result: AggregationResult,
    },
}

async fn run_query(
    State(state): State<AppState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<QueryResponse>, ApiError> {
    let q = params.q.unwrap_or_default();
    let parsed = parse_query(&q)?;
    let tantivy_query = plan_filter(state.store.index(), parsed.filter.as_ref())?;

    if let Some(agg) = parsed.aggregation {
        let result = execute_aggregation(
            &state.store,
            tantivy_query,
            params.from,
            params.to,
            &agg,
        )?;
        return Ok(Json(QueryResponse::Aggregation { result }));
    }

    let limit = params.limit.unwrap_or(100).clamp(1, 1000);
    let cursor = match params.cursor.as_deref() {
        Some(c) => Some(SearchCursor::decode(c).map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_cursor",
                "Invalid pagination cursor",
            )
        })?),
        None => None,
    };

    let result = state
        .store
        .search(EventSearchParams {
            query: tantivy_query,
            from: params.from,
            to: params.to,
            limit,
            cursor,
        })
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "search_failed",
                e.to_string(),
            )
        })?;

    Ok(Json(QueryResponse::Events(ListEventsResponse {
        events: result.events,
        next_cursor: result.next_cursor,
    })))
}
