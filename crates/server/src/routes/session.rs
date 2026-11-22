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
    // Before the lookup, so under load unknown and known usernames get the
    // same 429. Held until the response; each blocking step holds a clone,
    // so a client that disconnects mid-step frees its place only when that
    // step's work is done.
    let sign_in = auth::admit_sign_in(&state)?;
    let username = b.username.trim().to_string();
    let user = {
        let (u, held) = (username.clone(), sign_in.clone());
        state
            .db(move |m| {
                let _held = held;
                m.find_user(&u)
            })
            .await?
    };
    let ok = match &user {
        Some(u) => auth::check_password(&state, sign_in.clone(), b.password.clone(), u.password_hash.clone()).await?,
        None => false,
    };
    // An inactive account is refused like a wrong password, after the
    // same check, so the response does not tell which it was.
    let Some(user) = user.filter(|u| ok && u.active) else {
        // Slow down guessing; the response is identical for unknown users.
        // The place stays taken through the delay.
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(sign_in);
        return Err(ApiError::unauthorized("invalid username or password"));
    };
    let token = auth::random_token(32);
    let ttl = state.config.auth.session_ttl;
    let hash = auth::sha256_hex(&token);
    let uid = user.id;
    let held = sign_in.clone();
    let created = state
        .db(move |m| {
            let _held = held;
            match m.create_session(&hash, uid, ttl.as_millis() as i64) {
                // Deactivated or removed since the lookup above.
                Err(metadata::MetadataError::NotFound) => Ok(false),
                other => other.map(|()| true),
            }
        })
        .await?;
    if !created {
        drop(sign_in);
        return Err(ApiError::unauthorized("invalid username or password"));
    }
    let mut resp = Json(json!({ "authEnabled": true, "user": user.username })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, cookie(&token, ttl.as_secs()));
    drop(sign_in);
    Ok(resp)
}

pub async fn logout(State(state): State<SharedState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(token) = auth::session_from(&headers) {
        let hash = auth::sha256_hex(&token);
        let st = state.clone();
        state
            .blocking(move || {
                let deleted = st.metadata.delete_session(&hash);
                // After the delete, so no lookup can cache the session again,
                // and in the same blocking task, so a client that goes away
                // cannot skip it; also when the delete failed.
                st.sessions.invalidate(&hash);
                Ok(deleted?)
            })
            .await?;
    }
    let mut resp = Json(json!({ "ok": true })).into_response();
    resp.headers_mut().insert(header::SET_COOKIE, cookie("", 0));
    Ok(resp)
}

/// Public: tells the UI whether to show a login screen. `authEnabled` is
/// always true; it stays for clients written against 0.1.1.
pub async fn me(State(state): State<SharedState>, headers: HeaderMap) -> ApiResult<Response> {
    let user = match auth::session_from(&headers) {
        Some(token) => {
            let hash = auth::sha256_hex(&token);
            state.db(move |m| m.find_session(&hash)).await?.map(|u| u.username)
        }
        None => None,
    };
    Ok(Json(json!({ "authEnabled": true, "user": user })).into_response())
}
