mod accounts;

mod alerts;

mod auth;

mod auth_mw;

mod dashboards;

mod errors;

mod events;

mod filters;

mod metrics;

mod otlp_routes;

mod query_api;

mod settings;

mod traces;



use axum::middleware;

use axum::Router;



use crate::state::AppState;



use self::auth_mw::{require_ingest_auth, require_session};



pub fn router_with_state(state: AppState) -> Router<AppState> {

    let ingest = Router::new()

        .merge(events::ingest_router())

        .merge(metrics::ingest_router())

        .merge(otlp_routes::router())

        .layer(middleware::from_fn_with_state(

            state.clone(),

            require_ingest_auth,

        ));



    let protected = Router::new()

        .merge(events::query_router())

        .merge(query_api::router())

        .merge(traces::router())

        .merge(filters::router())

        .merge(dashboards::router())

        .merge(settings::router())

        .merge(metrics::query_router())

        .merge(alerts::router())

        .merge(auth::session_routes())

        .merge(accounts::router())

        .layer(middleware::from_fn_with_state(

            state.clone(),

            require_session,

        ));



    Router::new()

        .merge(auth::public_router())

        .merge(ingest)

        .merge(protected)

}



pub use alerts::evaluate_alerts;


#[cfg(test)]
mod tests;
