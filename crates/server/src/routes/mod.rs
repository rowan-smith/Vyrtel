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

use crate::auth::{self, Access};
use crate::state::SharedState;
use crate::web;

/// One operation: an HTTP method on a route, and what it needs from its
/// caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    pub method: &'static str,
    pub path: &'static str,
    pub access: Access,
}

const fn op(method: &'static str, path: &'static str, access: Access) -> Operation {
    Operation { method, path, access }
}

/// Every route [`router`] serves, with its scope (docs/api.md#operation-scopes).
/// A route's group in [`router`] decides its middleware; a test keeps this
/// list and the router identical, so no route is added without a scope.
/// Anything else is a static UI asset ([`web::static_handler`]), public
/// and separate from the API.
pub const OPERATIONS: &[Operation] = {
    use Access::{Admin, Ingest, Public};
    &[
        op("POST", "/api/v1/events", Ingest),
        op("POST", "/v1/logs", Ingest),
        op("POST", "/v1/traces", Ingest),
        op("POST", "/v1/metrics", Ingest),
        op("POST", "/api/v1/query/logs", Admin),
        op("POST", "/api/v1/query/traces", Admin),
        op("POST", "/api/v1/query/metrics", Admin),
        op("POST", "/api/v1/query/{signal}/events", Admin),
        op("POST", "/api/v1/query/{signal}/histogram", Admin),
        op("POST", "/api/v1/query/{signal}/count", Admin),
        op("POST", "/api/v1/query/{signal}/facets", Admin),
        op("GET", "/api/v1/live/logs", Admin),
        op("GET", "/api/v1/traces", Admin),
        op("GET", "/api/v1/traces/{trace_id}", Admin),
        op("GET", "/api/v1/metrics", Admin),
        op("GET", "/api/v1/system/info", Admin),
        op("GET", "/api/v1/system/storage", Admin),
        op("GET", "/api/v1/system/query-stats", Admin),
        op("GET", "/api/v1/system/config", Admin),
        op("GET", "/api/v1/dashboards", Admin),
        op("POST", "/api/v1/dashboards", Admin),
        op("GET", "/api/v1/dashboards/{id}", Admin),
        op("PUT", "/api/v1/dashboards/{id}", Admin),
        op("DELETE", "/api/v1/dashboards/{id}", Admin),
        op("POST", "/api/v1/dashboards/{id}/panels", Admin),
        op("PUT", "/api/v1/dashboards/{id}/panels/{panel_id}", Admin),
        op("DELETE", "/api/v1/dashboards/{id}/panels/{panel_id}", Admin),
        op("GET", "/api/v1/saved-queries", Admin),
        op("POST", "/api/v1/saved-queries", Admin),
        op("DELETE", "/api/v1/saved-queries/{id}", Admin),
        op("GET", "/api/v1/api-keys", Admin),
        op("POST", "/api/v1/api-keys", Admin),
        op("DELETE", "/api/v1/api-keys/{id}", Admin),
        op("PUT", "/api/v1/ingest-auth", Admin),
        op("GET", "/api/v1/alerts", Admin),
        op("POST", "/api/v1/alerts", Admin),
        op("GET", "/api/v1/alerts/{id}", Admin),
        op("PUT", "/api/v1/alerts/{id}", Admin),
        op("DELETE", "/api/v1/alerts/{id}", Admin),
        op("POST", "/api/v1/alerts/{id}/evaluate", Admin),
        op("GET", "/health", Public),
        op("GET", "/ready", Public),
        op("POST", "/api/v1/auth/login", Public),
        op("POST", "/api/v1/auth/logout", Public),
        op("GET", "/api/v1/auth/me", Public),
    ]
};

/// The HTTP API. Each group's middleware enforces its [`Access`]: `ingest`
/// is [`Access::Ingest`], `api` is [`Access::Admin`], `public` is
/// [`Access::Public`]. Keep [`OPERATIONS`] in step.
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
        .route("/api/v1/ingest-auth", axum::routing::put(manage::set_ingest_auth))
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

    // Ingest decodes its own bodies: every gzip member, with the size limit
    // applied to the decoded bytes (see `ingest::read_body`).
    let rest = Router::new().merge(public).merge(api).layer(RequestDecompressionLayer::new());

    Router::new()
        .merge(ingest)
        .merge(rest)
        .fallback(web::static_handler)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The operations [`router`] declares, read from this file: each
    /// `.route(...)` with the methods it chains, scoped by the group (and
    /// so the middleware) it is declared in.
    fn declared() -> BTreeSet<(String, String, &'static str)> {
        let source = include_str!("mod.rs");
        let body = &source[source.find("pub fn router(").unwrap()..source.find("#[cfg(test)]").unwrap()];
        let groups = [("let ingest", Access::Ingest), ("let api", Access::Admin), ("let public", Access::Public)];
        let mut found = BTreeSet::new();
        for (i, (start, access)) in groups.iter().enumerate() {
            let from = body.find(start).unwrap();
            let to =
                groups.get(i + 1).map_or_else(|| body.find("let rest").unwrap(), |(next, _)| body.find(next).unwrap());
            for route in body[from..to].split(".route(").skip(1) {
                let route = route.trim_start().strip_prefix('"').expect("a route path literal");
                let (path, rest) = route.split_once('"').unwrap();
                let call = rest.split(".route_layer(").next().unwrap();
                let chained = methods(call);
                assert!(!chained.is_empty(), "no method found for {path}");
                for method in chained {
                    found.insert((method, path.to_string(), access.as_str()));
                }
            }
        }
        found
    }

    /// The method routers called in `call`: `get(`, `.post(`,
    /// `routing::delete(` and so on, but not handlers like `get_dashboard(`.
    fn methods(call: &str) -> Vec<String> {
        let mut found = Vec::new();
        for name in ["get", "post", "put", "delete"] {
            for (at, _) in call.match_indices(&format!("{name}(")) {
                let before = call[..at].chars().next_back();
                if !before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    found.push(name.to_ascii_uppercase());
                }
            }
        }
        found
    }

    #[test]
    fn every_route_is_in_the_scope_inventory_and_nothing_else() {
        let listed: BTreeSet<_> =
            OPERATIONS.iter().map(|o| (o.method.to_string(), o.path.to_string(), o.access.as_str())).collect();
        assert_eq!(listed.len(), OPERATIONS.len(), "duplicate operations");
        assert_eq!(declared(), listed);
    }
}
