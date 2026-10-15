use axum::{http::StatusCode, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub database: &'static str,
    pub docker: &'static str,
}

pub async fn liveness() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "alive",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

#[derive(Clone)]
pub struct ReadinessState {
    pub db_ok: bool,
    pub docker_ok: bool,
}

pub async fn readiness(
    axum::extract::State(state): axum::extract::State<ReadinessState>,
) -> (StatusCode, Json<HealthResponse>) {
    let code = if state.db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    // Docker outage must NOT fail liveness, only degrade readiness.
    let docker = if state.docker_ok {
        "reachable"
    } else {
        "degraded"
    };
    (
        code,
        Json(HealthResponse {
            status: if state.db_ok { "ready" } else { "not-ready" },
            version: env!("CARGO_PKG_VERSION"),
            database: if state.db_ok { "ok" } else { "error" },
            docker,
        }),
    )
}
