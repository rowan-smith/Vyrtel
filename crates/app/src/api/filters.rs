use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/filters", get(list_filters).post(create_filter))
        .route("/api/filters/{id}", delete(delete_filter))
}

#[derive(Deserialize)]
struct CreateFilter {
    name: String,
    query: String,
}

async fn list_filters(
    State(state): State<AppState>,
) -> Result<Json<Vec<metadata::SavedFilter>>, ApiError> {
    state
        .metadata
        .list_saved_filters()
        .await
        .map(Json)
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                e.to_string(),
            )
        })
}

async fn create_filter(
    State(state): State<AppState>,
    Json(body): Json<CreateFilter>,
) -> Result<Json<metadata::SavedFilter>, ApiError> {
    if body.name.trim().is_empty() {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_request",
            "Name is required",
        ));
    }
    state
        .metadata
        .create_saved_filter(body.name.trim(), &body.query)
        .await
        .map(Json)
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                e.to_string(),
            )
        })
}

async fn delete_filter(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.metadata.delete_saved_filter(&id).await.map_err(|e| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "db_error",
            e.to_string(),
        )
    })?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
