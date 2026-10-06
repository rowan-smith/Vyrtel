use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub error: ApiErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct ApiErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
}

pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub position: Option<usize>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            position: None,
        }
    }

    pub fn with_position(mut self, position: usize) -> Self {
        self.position = Some(position);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            error: ApiErrorDetail {
                code: self.code,
                message: self.message,
                position: self.position,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<query::ParseError> for ApiError {
    fn from(e: query::ParseError) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, e.code, e.message).with_position(e.position)
    }
}

impl From<query::PlannerError> for ApiError {
    fn from(e: query::PlannerError) -> Self {
        match e {
            query::PlannerError::Parse(p) => p.into(),
            other => ApiError::new(StatusCode::BAD_REQUEST, "invalid_query", other.to_string()),
        }
    }
}
