use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::fmt;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub kind: &'static str,
    pub message: String,
}
impl ApiError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            kind: "invalid_request_error",
            message: message.into(),
        }
    }
    pub fn upstream(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            kind: "api_error",
            message: message.into(),
        }
    }
    pub fn timeout() -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            kind: "api_error",
            message: "Claude Code request timed out".into(),
        }
    }
    pub fn body(&self) -> Value {
        json!({"type":"error","error":{"type":self.kind,"message":self.message}})
    }
}
impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ApiError {}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body())).into_response()
    }
}
pub type Result<T> = std::result::Result<T, ApiError>;
