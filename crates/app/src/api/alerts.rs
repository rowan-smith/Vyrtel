use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use metadata::{AlertOperator, AlertStatus, AlertView};
use query::{execute_aggregation, parse_query, plan_filter, AggregationResult};
use serde::Deserialize;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/alerts", get(list_alerts).post(create_alert))
        .route("/api/alerts/{id}", delete(delete_alert))
        .route("/api/alerts/{id}/enabled", post(set_enabled))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateAlert {
    name: String,
    query: String,
    operator: String,
    threshold: f64,
}

#[derive(Deserialize)]
struct EnabledBody {
    enabled: bool,
}

async fn list_alerts(State(state): State<AppState>) -> Result<Json<Vec<AlertView>>, ApiError> {
    state
        .metadata
        .list_alert_views()
        .await
        .map(Json)
        .map_err(db_err)
}

async fn create_alert(
    State(state): State<AppState>,
    Json(body): Json<CreateAlert>,
) -> Result<Json<AlertView>, ApiError> {
    let operator = AlertOperator::parse(&body.operator).ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_request",
            "operator must be one of gt,gte,lt,lte,eq",
        )
    })?;
    // Validate query parses and has aggregation
    let parsed = parse_query(&body.query)?;
    if parsed.aggregation.is_none() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_query",
            "Alert queries must include an aggregation (e.g. | count)",
        ));
    }
    let rule = state
        .metadata
        .create_alert_rule(body.name.trim(), &body.query, operator, body.threshold)
        .await
        .map_err(db_err)?;
    Ok(Json(AlertView {
        rule,
        status: AlertStatus::Ok,
        value: None,
        message: None,
        last_evaluated_at: None,
        last_changed_at: None,
    }))
}

async fn delete_alert(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.metadata.delete_alert_rule(&id).await.map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn set_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<EnabledBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .metadata
        .set_alert_enabled(&id, body.enabled)
        .await
        .map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn db_err(e: anyhow::Error) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "db_error",
        e.to_string(),
    )
}

pub async fn evaluate_alerts(state: &AppState) -> anyhow::Result<()> {
    let rules = state.metadata.list_enabled_alert_rules().await?;
    let from = chrono::Utc::now() - chrono::Duration::hours(1);
    let to = chrono::Utc::now();

    for rule in rules {
        let parsed = match parse_query(&rule.query) {
            Ok(p) => p,
            Err(err) => {
                state
                    .metadata
                    .upsert_alert_state(
                        &rule.id,
                        AlertStatus::Ok,
                        None,
                        Some(&format!("invalid query: {}", err.message)),
                        false,
                    )
                    .await?;
                continue;
            }
        };
        let Some(agg) = parsed.aggregation else {
            continue;
        };
        let query = match plan_filter(state.store.index(), parsed.filter.as_ref()) {
            Ok(q) => q,
            Err(err) => {
                state
                    .metadata
                    .upsert_alert_state(
                        &rule.id,
                        AlertStatus::Ok,
                        None,
                        Some(&format!("plan error: {err}")),
                        false,
                    )
                    .await?;
                continue;
            }
        };
        let result = match execute_aggregation(&state.store, query, Some(from), Some(to), &agg) {
            Ok(r) => r,
            Err(err) => {
                state
                    .metadata
                    .upsert_alert_state(
                        &rule.id,
                        AlertStatus::Ok,
                        None,
                        Some(&format!("eval error: {err}")),
                        false,
                    )
                    .await?;
                continue;
            }
        };
        let value = match result {
            AggregationResult::Number { value } => value,
            AggregationResult::Groups { points } => {
                points.iter().map(|p| p.value).sum::<f64>()
            }
            AggregationResult::TimeSeries { points, .. } => {
                points.last().map(|p| p.value).unwrap_or(0.0)
            }
            AggregationResult::Table { rows } => rows.len() as f64,
        };
        let firing = rule.operator.compare(value, rule.threshold);
        let status = if firing {
            AlertStatus::Firing
        } else {
            AlertStatus::Ok
        };
        let previous = state.metadata.get_alert_status(&rule.id).await?;
        let changed = previous != status;
        let message = if firing {
            Some(format!(
                "{} {} {} (value {:.2})",
                rule.query,
                rule.operator.as_str(),
                rule.threshold,
                value
            ))
        } else {
            None
        };
        state
            .metadata
            .upsert_alert_state(
                &rule.id,
                status,
                Some(value),
                message.as_deref(),
                changed,
            )
            .await?;
    }
    Ok(())
}
