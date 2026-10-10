//! Structured API errors.
//!
//! Every error response has the shape
//! `{"error": {"code": "invalid_query", "message": "..."}}`. Messages are
//! written for clients; internal details (file paths, SQL errors) are logged
//! server-side and replaced with a generic message.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// Extra fields merged into the error object (e.g. query position).
    pub details: Option<serde_json::Value>,
    /// Send `Connection: close`: the request body was not read to the end,
    /// so the connection must not be reused for another request.
    pub close: bool,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), details: None, close: false }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", format!("{what} not found"))
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }

    /// Log the real cause, return a generic 500.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(error = %cause, "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "internal server error")
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn closing_connection(mut self) -> Self {
        self.close = true;
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut err = json!({ "code": self.code, "message": self.message });
        if let (Some(serde_json::Value::Object(extra)), Some(obj)) = (self.details, err.as_object_mut()) {
            obj.extend(extra);
        }
        let mut resp = (self.status, Json(json!({ "error": err }))).into_response();
        if self.status == StatusCode::TOO_MANY_REQUESTS || self.status == StatusCode::SERVICE_UNAVAILABLE {
            resp.headers_mut().insert(axum::http::header::RETRY_AFTER, axum::http::HeaderValue::from_static("1"));
        }
        if self.close {
            resp.headers_mut().insert(axum::http::header::CONNECTION, axum::http::HeaderValue::from_static("close"));
        }
        resp
    }
}

impl From<query::QueryError> for ApiError {
    fn from(e: query::QueryError) -> Self {
        use query::QueryError as Q;
        match e {
            Q::Parse(p) => ApiError::new(StatusCode::BAD_REQUEST, "invalid_query", p.message.clone())
                .with_details(json!({ "position": p.position })),
            Q::InvalidRequest(m) => ApiError::bad_request(m),
            Q::Timeout(_) => ApiError::new(StatusCode::GATEWAY_TIMEOUT, "query_timeout", e.to_string()),
            Q::TooExpensive(_) => ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "query_too_expensive", e.to_string()),
            Q::Storage(s) => s.into(),
        }
    }
}

impl From<storage::StorageError> for ApiError {
    fn from(e: storage::StorageError) -> Self {
        use storage::StorageError as S;
        match e {
            S::QueueFull => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "ingest queue is full; retry shortly")
            }
            S::BatchTooLarge(n) => ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                format!("batch of {n} events exceeds the ingest queue capacity; send smaller batches"),
            ),
            S::Unavailable(_) => {
                tracing::warn!(error = %e, "storage unavailable");
                ApiError::unavailable("storage is unavailable")
            }
            S::Corrupt { .. } => {
                tracing::error!(error = %e, "storage corruption detected during query");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage_error", "stored data could not be read")
            }
            other => ApiError::internal(other),
        }
    }
}

impl From<metadata::MetadataError> for ApiError {
    fn from(e: metadata::MetadataError) -> Self {
        match e {
            metadata::MetadataError::NotFound => ApiError::not_found("resource"),
            metadata::MetadataError::Invalid(m) => ApiError::bad_request(m),
            other => ApiError::internal(other),
        }
    }
}

impl From<ingest::IngestError> for ApiError {
    fn from(e: ingest::IngestError) -> Self {
        use ingest::IngestError as I;
        match &e {
            I::UnsupportedContentType(_) => {
                ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type", e.to_string())
            }
            I::Malformed(_) => ApiError::new(StatusCode::BAD_REQUEST, "invalid_payload", e.to_string()),
            I::InvalidEvent { index, .. } => ApiError::new(StatusCode::BAD_REQUEST, "invalid_event", e.to_string())
                .with_details(json!({ "index": index })),
        }
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        ApiError::internal(e)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

// Extractor wrappers so malformed JSON, paths and query strings produce the
// same structured error body as everything else.

#[derive(axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct ApiJson<T>(pub T);

#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct ApiPath<T>(pub T);

#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct ApiQuery<T>(pub T);

impl From<axum::extract::rejection::JsonRejection> for ApiError {
    fn from(r: axum::extract::rejection::JsonRejection) -> Self {
        let code =
            if r.status() == StatusCode::UNSUPPORTED_MEDIA_TYPE { "unsupported_media_type" } else { "invalid_json" };
        ApiError::new(r.status(), code, r.body_text())
    }
}

impl From<axum::extract::rejection::PathRejection> for ApiError {
    fn from(r: axum::extract::rejection::PathRejection) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text())
    }
}

impl From<axum::extract::rejection::QueryRejection> for ApiError {
    fn from(r: axum::extract::rejection::QueryRejection) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", r.body_text())
    }
}
