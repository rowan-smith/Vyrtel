//! Dashboards, saved queries and API keys (metadata CRUD).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use metadata::{PanelInput, PanelKind};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::SharedState;

#[derive(Deserialize)]
pub struct DashboardBody {
    pub name: String,
    pub description: Option<String>,
}

pub async fn list_dashboards(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let d = state.db(|m| m.list_dashboards()).await?;
    Ok(Json(json!({ "dashboards": d })))
}

pub async fn create_dashboard(
    State(state): State<SharedState>,
    ApiJson(b): ApiJson<DashboardBody>,
) -> ApiResult<Response> {
    let d = state.db(move |m| m.create_dashboard(&b.name, b.description.as_deref())).await?;
    Ok((StatusCode::CREATED, Json(d)).into_response())
}

pub async fn get_dashboard(State(state): State<SharedState>, ApiPath(id): ApiPath<i64>) -> ApiResult<Response> {
    let d = state.db(move |m| m.get_dashboard(id)).await.map_err(|_| ApiError::not_found("dashboard"))?;
    Ok(Json(d).into_response())
}

pub async fn update_dashboard(
    State(state): State<SharedState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(b): ApiJson<DashboardBody>,
) -> ApiResult<Response> {
    let d = state.db(move |m| m.update_dashboard(id, &b.name, b.description.as_deref())).await?;
    Ok(Json(d).into_response())
}

pub async fn delete_dashboard(State(state): State<SharedState>, ApiPath(id): ApiPath<i64>) -> ApiResult<StatusCode> {
    state.db(move |m| m.delete_dashboard(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Panel configs are free-form JSON, but the fields each kind needs are
/// checked here so a broken panel cannot be saved.
fn validate_panel(p: &PanelInput) -> ApiResult<()> {
    let has = |k: &str| p.config.get(k).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
    match p.kind {
        PanelKind::MetricLine if !has("metric") => Err(ApiError::bad_request("metric_line panels need config.metric")),
        PanelKind::SingleStat if !has("metric") && p.config.get("query").is_none_or(|q| !q.is_string()) => {
            Err(ApiError::bad_request("single_stat panels need config.query (log count) or config.metric"))
        }
        _ => {
            if let Some(q) = p.config.get("query").and_then(Value::as_str) {
                query::parse(q).map_err(|e| {
                    ApiError::new(StatusCode::BAD_REQUEST, "invalid_query", e.message.clone())
                        .with_details(json!({ "position": e.position }))
                })?;
            }
            Ok(())
        }
    }
}

pub async fn add_panel(
    State(state): State<SharedState>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(p): ApiJson<PanelInput>,
) -> ApiResult<Response> {
    validate_panel(&p)?;
    let panel = state.db(move |m| m.add_panel(id, &p)).await?;
    Ok((StatusCode::CREATED, Json(panel)).into_response())
}

pub async fn update_panel(
    State(state): State<SharedState>,
    ApiPath((id, panel_id)): ApiPath<(i64, i64)>,
    ApiJson(p): ApiJson<PanelInput>,
) -> ApiResult<Response> {
    validate_panel(&p)?;
    let panel = state.db(move |m| m.update_panel(id, panel_id, &p)).await?;
    Ok(Json(panel).into_response())
}

pub async fn delete_panel(
    State(state): State<SharedState>,
    ApiPath((id, panel_id)): ApiPath<(i64, i64)>,
) -> ApiResult<StatusCode> {
    state.db(move |m| m.delete_panel(id, panel_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SavedQueryBody {
    pub name: String,
    #[serde(default = "logs")]
    pub signal: String,
    pub query: String,
}

fn logs() -> String {
    "logs".into()
}

pub async fn list_saved_queries(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let q = state.db(|m| m.list_saved_queries()).await?;
    Ok(Json(json!({ "savedQueries": q })))
}

pub async fn create_saved_query(
    State(state): State<SharedState>,
    ApiJson(b): ApiJson<SavedQueryBody>,
) -> ApiResult<Response> {
    query::parse(&b.query).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "invalid_query", e.message))?;
    let q = state.db(move |m| m.create_saved_query(&b.name, &b.signal, &b.query)).await?;
    Ok((StatusCode::CREATED, Json(q)).into_response())
}

pub async fn delete_saved_query(State(state): State<SharedState>, ApiPath(id): ApiPath<i64>) -> ApiResult<StatusCode> {
    state.db(move |m| m.delete_saved_query(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ApiKeyBody {
    pub name: String,
    #[serde(default = "ingest_scope")]
    pub scope: String,
}

fn ingest_scope() -> String {
    "ingest".into()
}

pub async fn list_api_keys(State(state): State<SharedState>) -> ApiResult<Json<Value>> {
    let k = state.db(|m| m.list_api_keys()).await?;
    Ok(Json(json!({ "apiKeys": k })))
}

/// The plaintext key appears in this response only.
pub async fn create_api_key(State(state): State<SharedState>, ApiJson(b): ApiJson<ApiKeyBody>) -> ApiResult<Response> {
    let (key, prefix, hash) = auth::new_api_key();
    let rec = state.db(move |m| m.create_api_key(&b.name, &prefix, &hash, &b.scope)).await?;
    let mut v = serde_json::to_value(rec).map_err(ApiError::internal)?;
    v["key"] = Value::String(key);
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

pub async fn revoke_api_key(State(state): State<SharedState>, ApiPath(id): ApiPath<i64>) -> ApiResult<StatusCode> {
    state.db(move |m| m.revoke_api_key(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
