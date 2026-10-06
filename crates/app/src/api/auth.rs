use axum::extract::State;

use axum::http::{header, HeaderMap, StatusCode};

use axum::routing::{get, post};

use axum::{Extension, Json, Router};

use metadata::Account;

use serde::{Deserialize, Serialize};

use crate::api::auth_mw::AuthUser;

use crate::api::errors::ApiError;

use crate::state::AppState;

pub fn public_router() -> Router<AppState> {
    Router::new().route("/api/auth/login", post(login))
}

pub fn session_routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/me", get(me))
        .route("/api/auth/logout", post(logout))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]

struct LoginRequest {
    username: String,

    password: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]

struct LoginResponse {
    token: String,

    account: Account,

    expires_at: chrono::DateTime<chrono::Utc>,
}

async fn login(
    State(state): State<AppState>,

    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    let username = body.username.trim();

    if username.is_empty() || body.password.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Username and password are required",
        ));
    }

    let account = state
        .metadata
        .verify_login(username, &body.password)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "db_error", e.to_string()))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Invalid username or password",
            )
        })?;

    let session = state
        .metadata
        .create_session(&account.id, 24 * 7)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "db_error", e.to_string()))?;

    Ok(Json(LoginResponse {
        token: session.token,

        account,

        expires_at: session.expires_at,
    }))
}

async fn me(Extension(user): Extension<AuthUser>) -> Json<Account> {
    Json(user.0)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<StatusCode, ApiError> {
    if let Some(token) = headers
        .get("x-observatory-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let _ = state.metadata.delete_session(token).await;
    } else if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            let _ = state.metadata.delete_session(token.trim()).await;
        }
    }

    Ok(StatusCode::NO_CONTENT)
}
