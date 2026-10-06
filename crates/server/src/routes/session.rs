//! Login / logout / current user.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{self, SESSION_COOKIE};
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::state::SharedState;

#[derive(Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

fn cookie(value: &str, max_age: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}"))
        .expect("cookie header is ASCII")
}

pub async fn login(State(state): State<SharedState>, ApiJson(b): ApiJson<LoginBody>) -> ApiResult<Response> {
    if !state.config.auth.enabled {
        return Ok(Json(json!({ "authEnabled": false, "user": null })).into_response());
    }
    let username = b.username.trim().to_string();
    let user = {
        let u = username.clone();
        state.db(move |m| m.find_user(&u)).await?
    };
    let ok = match &user {
        Some(u) => {
            let hash = u.password_hash.clone();
            let pw = b.password.clone();
            state.blocking(move || Ok(auth::verify_password(&pw, &hash))).await?
        }
        None => false,
    };
    let Some(user) = user.filter(|_| ok) else {
        // Slow down guessing; the response is identical for unknown users.
        tokio::time::sleep(Duration::from_millis(400)).await;
        return Err(ApiError::unauthorized("invalid username or password"));
    };
    let token = auth::random_token(32);
    let ttl = state.config.auth.session_ttl;
    let hash = auth::sha256_hex(&token);
    let uid = user.id;
    state.db(move |m| m.create_session(&hash, uid, ttl.as_millis() as i64)).await?;
    let mut resp = Json(json!({ "authEnabled": true, "user": user.username })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, cookie(&token, ttl.as_secs()));
    Ok(resp)
}

pub async fn logout(State(state): State<SharedState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(token) = auth::session_from(&headers) {
        let hash = auth::sha256_hex(&token);
        state.db(move |m| m.delete_session(&hash)).await?;
    }
    let mut resp = Json(json!({ "ok": true })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, cookie("", 0));
    Ok(resp)
}

/// Public: tells the UI whether to show a login screen.
pub async fn me(State(state): State<SharedState>, headers: HeaderMap) -> ApiResult<Response> {
    if !state.config.auth.enabled {
        return Ok(Json(json!({ "authEnabled": false, "user": null })).into_response());
    }
    let user = match auth::session_from(&headers) {
        Some(token) => {
            let hash = auth::sha256_hex(&token);
            state.db(move |m| m.find_session(&hash)).await?.map(|u| u.username)
        }
        None => None,
    };
    Ok(Json(json!({ "authEnabled": true, "user": user })).into_response())
}
