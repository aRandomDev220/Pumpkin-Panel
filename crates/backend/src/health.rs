use std::time::Duration;

use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;

use crate::AppState;

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

/// Readiness probes the database and Docker on every call. A boot-time
/// snapshot is not enough: a manager that loses either dependency must stop
/// reporting itself ready.
pub async fn readiness(State(app): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let db_ok = tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&app.pool),
    )
    .await
    .is_ok_and(|result| result.is_ok());

    let docker_ok = match &app.adapter {
        Some(adapter) => tokio::time::timeout(Duration::from_secs(1), adapter.ping())
            .await
            .is_ok_and(|result| result.is_ok()),
        None => false,
    };

    readiness_response(db_ok, docker_ok)
}

fn readiness_response(db_ok: bool, docker_ok: bool) -> (StatusCode, Json<HealthResponse>) {
    let ready = db_ok && docker_ok;
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(HealthResponse {
            status: if ready { "ready" } else { "not-ready" },
            version: env!("CARGO_PKG_VERSION"),
            database: if db_ok { "ok" } else { "error" },
            docker: if docker_ok { "reachable" } else { "degraded" },
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_requires_database_and_docker() {
        assert_eq!(readiness_response(true, true).0, StatusCode::OK);
        assert_eq!(
            readiness_response(false, true).0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            readiness_response(true, false).0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            readiness_response(false, false).0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
