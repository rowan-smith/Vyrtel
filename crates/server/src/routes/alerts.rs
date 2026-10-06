use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use metadata::AlertInput;
use serde_json::{Value, json};

use crate::alerts::evaluate_and_record;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::SharedState;

fn check_query(a: &AlertInput) -> ApiResult<()> {
    query::parse(&a.query).map(|_| ()).map_err(|e| {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid_query", e.message.clone())
            .with_details(json!({ "position": e.position }))
    })
}

pub async fn list(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let a = state.db(|m| m.list_alerts()).await?;
    Ok(Json(json!({ "alerts": a })))
}

pub async fn create(State(state): State<SharedState>, ApiJson(a): ApiJson<AlertInput>) -> ApiResult<Response> {
    check_query(&a)?;
    let alert = state.db(move |m| m.create_alert(&a)).await?;
    Ok((StatusCode::CREATED, Json(alert)).into_response())
}

pub async fn get(State(state): State<SharedState>, ApiPath(id): ApiPath<i64>) -> ApiResult<Json<Value>> {
    let (alert, history) = state
        .db(move |m| Ok((m.get_alert(id)?, m.alert_history(id, 50)?)))
        .await
        .map_err(|e| if e.code == "not_found" { ApiError::not_found("alert") } else { e })?;
    let mut v = serde_json::to_value(alert).map_err(ApiError::internal)?;
    v["history"] = serde_json::to_value(history).map_err(ApiError::internal)?;
    Ok(Json(v))
}

pub async fn update(
    State(state): State<SharedState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(a): ApiJson<AlertInput>,
) -> ApiResult<Response> {
    check_query(&a)?;
    let alert = state.db(move |m| m.update_alert(id, &a)).await?;
    Ok(Json(alert).into_response())
}

pub async fn delete(State(state): State<SharedState>, ApiPath(id): ApiPath<i64>) -> ApiResult<StatusCode> {
    state.db(move |m| m.delete_alert(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Evaluate immediately (handy for testing an alert from the UI).
pub async fn evaluate(State(state): State<SharedState>, ApiPath(id): ApiPath<i64>) -> ApiResult<Response> {
    let alert = state.db(move |m| m.get_alert(id)).await?;
    let updated = evaluate_and_record(&state, &alert).await?;
    Ok(Json(updated).into_response())
}
