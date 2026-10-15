use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // wired to routes in Phase 2
pub enum ApiError {
    #[error("internal error")]
    Internal(#[from] anyhow::Error),
}

#[derive(Serialize)]
#[allow(dead_code)] // wired to routes in Phase 2
struct ErrorBody {
    error_code: &'static str,
    message: String,
    request_id: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = uuid::Uuid::new_v4().to_string();
        tracing::error!(%request_id, error = %self, "request failed");
        let body = ErrorBody {
            error_code: "INTERNAL",
            message: "Internal server error".into(),
            request_id,
        };
        (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}
