//! The web UI, embedded into the binary at build time (see build.rs).

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::error::ApiError;

#[derive(RustEmbed)]
#[folder = "$VYRTEL_WEB_DIST"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    // Unknown API routes get a JSON 404, not the SPA shell.
    if path.starts_with("api/") || path.starts_with("v1/") {
        return ApiError::not_found("endpoint").into_response();
    }
    if let Some(resp) = asset(path) {
        return resp;
    }
    // Client-side routes (/logs, /traces/abc...) all serve index.html.
    if path.contains('.') && !path.ends_with(".html") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    asset("index.html").unwrap_or_else(|| (StatusCode::NOT_FOUND, "UI not built").into_response())
}

fn asset(path: &str) -> Option<Response> {
    let path = if path.is_empty() { "index.html" } else { path };
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Vite emits content-hashed file names under assets/, safe to cache.
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    Some(
        ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())], file.data)
            .into_response(),
    )
}
