use axum::extract::{Path, Query, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use metadata::{Dashboard, DashboardWidget, Visualization, WidgetPosition};
use query::{execute_aggregation, parse_query, plan_filter};
use serde::Deserialize;

use crate::api::errors::ApiError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/dashboards", get(list_dashboards).post(create_dashboard))
        .route(
            "/api/dashboards/{id}",
            get(get_dashboard).delete(delete_dashboard).put(update_dashboard),
        )
        .route(
            "/api/dashboards/{id}/widgets",
            get(list_widgets).post(create_widget),
        )
        .route(
            "/api/dashboards/{dashboard_id}/widgets/{widget_id}",
            put(update_widget).delete(delete_widget),
        )
        .route("/api/dashboards/{id}/query", get(run_widget_query))
}

#[derive(Deserialize)]
struct CreateDashboard {
    name: String,
}

#[derive(Deserialize)]
struct UpdateDashboard {
    name: String,
}

#[derive(Deserialize)]
struct CreateWidget {
    title: String,
    query: String,
    visualization: Visualization,
    #[serde(default)]
    position: Option<WidgetPosition>,
}

#[derive(Deserialize)]
struct UpdateWidget {
    title: String,
    query: String,
    visualization: Visualization,
    position: WidgetPosition,
}

#[derive(Deserialize)]
struct WidgetQueryParams {
    q: String,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}

async fn list_dashboards(State(state): State<AppState>) -> Result<Json<Vec<Dashboard>>, ApiError> {
    state.metadata.list_dashboards().await.map(Json).map_err(db_err)
}

async fn create_dashboard(
    State(state): State<AppState>,
    Json(body): Json<CreateDashboard>,
) -> Result<Json<Dashboard>, ApiError> {
    state
        .metadata
        .create_dashboard(body.name.trim())
        .await
        .map(Json)
        .map_err(db_err)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardDetail {
    #[serde(flatten)]
    dashboard: Dashboard,
    widgets: Vec<DashboardWidget>,
}

async fn get_dashboard(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DashboardDetail>, ApiError> {
    let dashboard = state
        .metadata
        .get_dashboard(&id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| {
            ApiError::new(axum::http::StatusCode::NOT_FOUND, "not_found", "Dashboard not found")
        })?;
    let widgets = state.metadata.list_widgets(&id).await.map_err(db_err)?;
    Ok(Json(DashboardDetail { dashboard, widgets }))
}

async fn update_dashboard(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateDashboard>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .metadata
        .rename_dashboard(&id, body.name.trim())
        .await
        .map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_dashboard(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.metadata.delete_dashboard(&id).await.map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_widgets(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<DashboardWidget>>, ApiError> {
    state.metadata.list_widgets(&id).await.map(Json).map_err(db_err)
}

async fn create_widget(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CreateWidget>,
) -> Result<Json<DashboardWidget>, ApiError> {
    let position = body.position.unwrap_or(WidgetPosition {
        x: 0,
        y: 0,
        w: 1,
        h: 1,
    });
    state
        .metadata
        .create_widget(&id, &body.title, &body.query, body.visualization, position)
        .await
        .map(Json)
        .map_err(db_err)
}

async fn update_widget(
    State(state): State<AppState>,
    Path((_dashboard_id, widget_id)): Path<(String, String)>,
    Json(body): Json<UpdateWidget>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state
        .metadata
        .update_widget(
            &widget_id,
            &body.title,
            &body.query,
            body.visualization,
            body.position,
        )
        .await
        .map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_widget(
    State(state): State<AppState>,
    Path((_dashboard_id, widget_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    state.metadata.delete_widget(&widget_id).await.map_err(db_err)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn run_widget_query(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Query(params): Query<WidgetQueryParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let parsed = parse_query(&params.q)?;
    let tantivy_query = plan_filter(state.store.index(), parsed.filter.as_ref())?;
    let Some(agg) = parsed.aggregation else {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_query",
            "Dashboard queries require an aggregation (e.g. | count)",
        ));
    };
    let result = execute_aggregation(&state.store, tantivy_query, params.from, params.to, &agg)?;
    Ok(Json(serde_json::json!({ "result": result })))
}

fn db_err(e: anyhow::Error) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "db_error",
        e.to_string(),
    )
}
