use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mvm_common::api::ErrorResponse;
use mvm_common::Error;

pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self(Error::Io(e))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::SandboxNotFound(_) | Error::ImageNotFound(_) => StatusCode::NOT_FOUND,
            Error::InvalidState(_) => StatusCode::CONFLICT,
            Error::Other(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(ErrorResponse {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}
