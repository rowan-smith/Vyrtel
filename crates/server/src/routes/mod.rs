//! HTTP routing. See docs/api.md for the full reference.

pub mod alerts;
pub mod ingest;
pub mod live;
pub mod manage;
pub mod query;
pub mod session;
pub mod system;

use axum::Router;
use axum::http::{HeaderValue, header};
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::auth;
use crate::state::SharedState;
use crate::web;

pub fn router(state: SharedState) -> Router {
    let ingest = Router::new()
        .route("/api/v1/events", post(ingest::native_events))
        .route("/v1/logs", post(ingest::otlp_logs))
        .route("/v1/traces", post(ingest::otlp_traces))
        .route("/v1/metrics", post(ingest::otlp_metrics))
        .route_layer(from_fn_with_state(state.clone(), auth::require_ingest));

    let api = Router::new()
        .route("/api/v1/query/logs", post(query::logs))
        .route("/api/v1/query/traces", post(query::trace_search))
        .route("/api/v1/query/metrics", post(query::metric_query))
        .route("/api/v1/query/{signal}/events", post(query::events))
        .route("/api/v1/query/{signal}/histogram", post(query::histogram))
        .route("/api/v1/query/{signal}/count", post(query::count))
        .route("/api/v1/query/{signal}/facets", post(query::facets))
        .route("/api/v1/live/logs", get(live::live))
        .route("/api/v1/live", get(live::live))
        .route("/api/v1/traces", get(query::trace_search_get))
        .route("/api/v1/traces/{trace_id}", get(query::trace_get))
        .route("/api/v1/metrics", get(query::metric_names))
        .route("/api/v1/system/info", get(system::info))
        .route("/api/v1/system/storage", get(system::storage_stats))
        .route("/api/v1/system/query-stats", get(system::query_stats))
        .route("/api/v1/system/config", get(system::config))
        .route("/api/v1/dashboards", get(manage::list_dashboards).post(manage::create_dashboard))
        .route(
            "/api/v1/dashboards/{id}",
            get(manage::get_dashboard).put(manage::update_dashboard).delete(manage::delete_dashboard),
        )
        .route("/api/v1/dashboards/{id}/panels", post(manage::add_panel))
        .route(
            "/api/v1/dashboards/{id}/panels/{panel_id}",
            axum::routing::put(manage::update_panel).delete(manage::delete_panel),
        )
        .route("/api/v1/saved-queries", get(manage::list_saved_queries).post(manage::create_saved_query))
        .route("/api/v1/saved-queries/{id}", axum::routing::delete(manage::delete_saved_query))
        .route("/api/v1/api-keys", get(manage::list_api_keys).post(manage::create_api_key))
        .route("/api/v1/api-keys/{id}", axum::routing::delete(manage::revoke_api_key))
        .route("/api/v1/alerts", get(alerts::list).post(alerts::create))
        .route("/api/v1/alerts/{id}", get(alerts::get).put(alerts::update).delete(alerts::delete))
        .route("/api/v1/alerts/{id}/evaluate", post(alerts::evaluate))
        .route_layer(from_fn_with_state(state.clone(), auth::require_user));

    let public = Router::new()
        .route("/health", get(system::health))
        .route("/ready", get(system::ready))
        .route("/api/v1/auth/login", post(session::login))
        .route("/api/v1/auth/logout", post(session::logout))
        .route("/api/v1/auth/me", get(session::me));

    Router::new()
        .merge(public)
        .merge(ingest)
        .merge(api)
        .fallback(web::static_handler)
        // OTLP exporters commonly gzip their payloads.
        .layer(RequestDecompressionLayer::new())
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .with_state(state)
}
