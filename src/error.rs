use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub struct Error(pub StatusCode, pub String);
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn bad(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    pub fn unauthorized() -> Self {
        Self(StatusCode::UNAUTHORIZED, "Authentication required".into())
    }
    pub fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, "Not found".into())
    }
    pub fn upstream(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_GATEWAY, message.into())
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "Database operation failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database operation failed".into(),
        )
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        tracing::error!(%error, "Storage operation failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Storage operation failed".into(),
        )
    }
}
impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(%error, "Operation failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "Operation failed".into())
    }
}
