use axum::extract::Request;
use axum::extract::State;
use axum::http::header::{AUTHORIZATION, HeaderMap};
use axum::middleware::Next;
use axum::response::Response;
use metadata::Account;

use crate::api::errors::ApiError;
use crate::state::AppState;

const SESSION_HEADER: &str = "x-observatory-token";
const API_KEY_HEADER: &str = "x-api-key";

#[derive(Clone, Debug)]
pub struct AuthUser(pub Account);

pub async fn require_session(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let token = extract_session_token(req.headers())
        .or_else(|| extract_token_from_query(req.uri().query()))
        .ok_or_else(unauthorized)?;
    let account = state
        .metadata
        .get_session_account(&token)
        .await
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                e.to_string(),
            )
        })?
        .ok_or_else(unauthorized)?;
    req.extensions_mut().insert(AuthUser(account));
    Ok(next.run(req).await)
}

pub async fn require_ingest_auth(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if state.config.development {
        return Ok(next.run(req).await);
    }

    let raw_key = extract_api_key(req.headers()).ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            "API key required (X-Api-Key or Authorization: Bearer)",
        )
    })?;

    let account = state
        .metadata
        .find_account_by_api_key(&raw_key)
        .await
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "db_error",
                e.to_string(),
            )
        })?
        .ok_or_else(|| {
            ApiError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Invalid API key",
            )
        })?;

    req.extensions_mut().insert(AuthUser(account));
    Ok(next.run(req).await)
}

fn unauthorized() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Sign in required",
    )
}

fn extract_token_from_query(query: Option<&str>) -> Option<String> {
    let q = query?;
    for pair in q.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == "token" {
            let value = parts.next().unwrap_or("");
            let decoded = urlencoding_decode(value);
            if !decoded.is_empty() && !decoded.starts_with("obs_") {
                return Some(decoded);
            }
        }
    }
    None
}

fn urlencoding_decode(s: &str) -> String {
    // Percent-decode enough for UUID tokens; fall back to raw.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let h = from_hex(bytes[i + 1]);
                let l = from_hex(bytes[i + 2]);
                if let (Some(h), Some(l)) = (h, l) {
                    out.push((h << 4) | l);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(v) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if let Some(token) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            let token = token.trim();
            if !token.starts_with("obs_") && !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(API_KEY_HEADER).and_then(|v| v.to_str().ok()) {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(v) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if let Some(token) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
        if let Some(token) = v.strip_prefix("ApiKey ").or_else(|| v.strip_prefix("Api-Key ")) {
            let token = token.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn session_token_from_headers() {
        let token = |pairs: &[(&'static str, &str)]| extract_session_token(&headers(pairs));
        assert_eq!(token(&[(SESSION_HEADER, " abc ")]).as_deref(), Some("abc"));
        assert_eq!(token(&[("authorization", "Bearer abc")]).as_deref(), Some("abc"));
        assert_eq!(token(&[("authorization", "bearer abc")]).as_deref(), Some("abc"));
        // The custom header wins over Authorization.
        assert_eq!(
            token(&[(SESSION_HEADER, "one"), ("authorization", "Bearer two")]).as_deref(),
            Some("one")
        );
        // API keys are never treated as session tokens.
        assert_eq!(token(&[("authorization", "Bearer obs_key")]), None);
        assert_eq!(token(&[("authorization", "Basic abc")]), None);
        assert_eq!(token(&[(SESSION_HEADER, "   ")]), None);
        assert_eq!(token(&[]), None);
    }

    #[test]
    fn api_key_from_headers() {
        let key = |pairs: &[(&'static str, &str)]| extract_api_key(&headers(pairs));
        assert_eq!(key(&[(API_KEY_HEADER, "obs_1")]).as_deref(), Some("obs_1"));
        assert_eq!(key(&[("authorization", "Bearer obs_1")]).as_deref(), Some("obs_1"));
        assert_eq!(key(&[("authorization", "ApiKey obs_1")]).as_deref(), Some("obs_1"));
        assert_eq!(key(&[("authorization", "Api-Key obs_1")]).as_deref(), Some("obs_1"));
        assert_eq!(key(&[("authorization", "Bearer ")]), None);
        assert_eq!(key(&[(API_KEY_HEADER, "")]), None);
        assert_eq!(key(&[]), None);
    }

    #[test]
    fn token_from_query_string() {
        assert_eq!(extract_token_from_query(Some("token=abc")).as_deref(), Some("abc"));
        assert_eq!(extract_token_from_query(Some("a=1&token=abc&b=2")).as_deref(), Some("abc"));
        assert_eq!(extract_token_from_query(Some("token=a%2Db")).as_deref(), Some("a-b"));
        assert_eq!(extract_token_from_query(Some("token=")), None);
        assert_eq!(extract_token_from_query(Some("token=obs_key")), None);
        assert_eq!(extract_token_from_query(Some("tokens=abc")), None);
        assert_eq!(extract_token_from_query(None), None);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(urlencoding_decode("a%20b+c"), "a b c");
        assert_eq!(urlencoding_decode("%41%62"), "Ab");
        assert_eq!(urlencoding_decode("a%2D"), "a-");
        assert_eq!(urlencoding_decode("100%"), "100%");
        assert_eq!(urlencoding_decode("%zz"), "%zz");
        assert_eq!(urlencoding_decode("%e2%98%95"), "☕");
    }
}
