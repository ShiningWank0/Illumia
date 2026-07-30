use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "request failed");
        Self::internal_silent()
    }

    /// Produces a generic 500 without recording vault-sensitive error context.
    ///
    /// `vault: no-log`
    pub fn internal_silent() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal server error",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

impl From<illumia_core::db::Error> for ApiError {
    fn from(error: illumia_core::db::Error) -> Self {
        match error {
            illumia_core::db::Error::AssetNotFound => Self::not_found("asset not found"),
            illumia_core::db::Error::StackNotFound => Self::not_found("manga stack not found"),
            illumia_core::db::Error::StackChapterNotFound => {
                Self::not_found("stack chapter not found")
            }
            illumia_core::db::Error::InvalidBucketKey
            | illumia_core::db::Error::InvalidStack(_)
            | illumia_core::db::Error::UnsupportedExtension(_)
            | illumia_core::db::Error::InvalidSetting(_)
            | illumia_core::db::Error::InvalidJobProgress
            | illumia_core::db::Error::InvalidJobState(_) => Self::bad_request(error.to_string()),
            _ => Self::internal(error),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
