use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist"]
#[prefix = ""]
#[exclude = "*.map"]
struct Assets;

pub async fn static_handler(uri: Uri, req: Request) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    // Don't hijack API routes (shouldn't reach here, but be safe)
    if path.starts_with("api/") || path.starts_with("v1/") {
        return StatusCode::NOT_FOUND.into_response();
    }

    if path.is_empty() || path == "index.html" {
        return serve_index();
    }

    if let Some(content) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .body(Body::from(content.data.to_vec()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // SPA fallback for client-side routes
    if req.method() == axum::http::Method::GET && !path.contains('.') {
        return serve_index();
    }

    StatusCode::NOT_FOUND.into_response()
}

fn serve_index() -> Response {
    match Assets::get("index.html") {
        Some(content) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(content.data.to_vec()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        None => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(PLACEHOLDER_HTML))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

const PLACEHOLDER_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<title>Observatory</title>
<style>
body{font-family:system-ui,sans-serif;background:#1a1a1a;color:#ddd;display:flex;align-items:center;justify-content:center;height:100vh;margin:0}
main{max-width:36rem;padding:2rem}
code{background:#2a2a2a;padding:0.15rem 0.4rem;border-radius:3px}
</style>
</head>
<body>
<main>
<h1>Observatory</h1>
<p>Frontend assets are not built yet. From <code>web/</code> run:</p>
<pre>npm install && npm run build</pre>
<p>API is available at <code>/api/events</code>.</p>
</main>
</body>
</html>"#;
