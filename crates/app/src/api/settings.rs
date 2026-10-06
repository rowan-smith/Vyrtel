use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use metadata::AppSettings;
use serde::Deserialize;
use serde_json::Value;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/settings", get(get_settings).put(update_settings))
        .route("/api/about", get(about))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateSettings {
    /// `None` when the field is omitted; `Some(Value::Null)` when it is explicitly `null`.
    #[serde(default, deserialize_with = "present")]
    retention_days: Option<Value>,
}

fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

async fn get_settings(State(state): State<AppState>) -> Result<Json<AppSettings>, ApiError> {
    state.metadata.get_settings().await.map(Json).map_err(|e| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            e.to_string(),
        )
    })
}

async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<UpdateSettings>,
) -> Result<Json<AppSettings>, ApiError> {
    if let Some(value) = body.retention_days {
        let invalid = || {
            ApiError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid_request",
                "retentionDays must be a whole number, null, or \"forever\"",
            )
        };
        let days = match value {
            Value::Null => None,
            Value::Number(n) => Some(n.as_i64().ok_or_else(invalid)?),
            Value::String(s) if s.eq_ignore_ascii_case("forever") => None,
            Value::String(s) => Some(s.trim().parse().map_err(|_| invalid())?),
            _ => return Err(invalid()),
        };
        state.metadata.set_retention_days(days).await.map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                e.to_string(),
            )
        })?;
    }
    get_settings(State(state)).await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AboutInfo {
    name: String,
    version: &'static str,
    event_count: usize,
    storage_path: String,
    development: bool,
}

async fn about(State(state): State<AppState>) -> Result<Json<AboutInfo>, ApiError> {
    let event_count = state.store.event_count().unwrap_or(0);
    Ok(Json(AboutInfo {
        name: state.config.name.clone(),
        version: env!("CARGO_PKG_VERSION"),
        event_count,
        storage_path: state.config.storage.path.display().to_string(),
        development: state.config.development,
    }))
}
