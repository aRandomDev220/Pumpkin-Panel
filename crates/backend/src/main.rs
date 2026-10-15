mod config;
mod db;
mod error;
mod health;

use axum::{routing::get, Router};
use std::path::PathBuf;
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = config::Settings::load().unwrap_or_else(|e| {
        eprintln!("invalid configuration: {e:#}");
        std::process::exit(1);
    });

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| settings.log_level.clone().into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    // Ensure writable dirs exist before DB connect (handles fresh volumes).
    for dir in [&settings.data_dir, &settings.backup_dir] {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(parent) = db_path_parent(&settings.database_url) {
        std::fs::create_dir_all(parent)?;
    }

    let pool = db::connect(&settings.database_url).await.map_err(|e| {
        tracing::error!(error = %e, "database connect/migrate failed");
        e
    })?;
    let db_ok = sqlx::query("SELECT 1").execute(&pool).await.is_ok();
    // Docker reachability check is best-effort for Phase 1 (socket may not
    // exist in dev). Full Docker adapter lands in Phase 2.
    let docker_ok = std::path::Path::new("/var/run/docker.sock").exists();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        port = settings.http_port,
        db_ok,
        docker_ok,
        "pumpkin-manager starting"
    );

    let readiness = health::ReadinessState { db_ok, docker_ok };
    let app = Router::new()
        .route("/api/v1/health/live", get(health::liveness))
        .route(
            "/api/v1/health/ready",
            get(health::readiness).with_state(readiness),
        )
        .route(
            "/api/v1/servers",
            get(|| async { axum::Json(serde_json::json!({"servers": []})) }),
        )
        .layer(RequestBodyLimitLayer::new(5 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .fallback(static_handler);

    let addr = settings.bind_addr();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Serves the Vite build (`apps/web/dist`) when bundled in the single
/// Docker image, otherwise returns 404 JSON so the API still works in dev.
async fn static_handler(req: axum::http::Request<axum::body::Body>) -> axum::response::Response {
    use axum::http::StatusCode;

    let path = req.uri().path().to_owned();
    // Never shadow API routes.
    if path.starts_with("/api/") {
        return (StatusCode::NOT_FOUND, "{\"error_code\":\"NOT_FOUND\"}").into_response();
    }
    let dist = PathBuf::from("/app/web");
    let file = if path == "/" {
        dist.join("index.html")
    } else {
        dist.join(path.trim_start_matches('/'))
    };
    // Path traversal guard.
    if !file.starts_with(&dist) || !file.is_file() {
        let index = dist.join("index.html");
        if index.is_file() {
            let body = std::fs::read(&index).unwrap_or_default();
            return ([("content-type", "text/html")], body).into_response();
        }
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let body = std::fs::read(&file).unwrap_or_default();
    let ct = mime_for(&file);
    ([("content-type", ct)], body).into_response()
}

fn mime_for(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

use axum::response::IntoResponse;

fn db_path_parent(url: &str) -> Option<PathBuf> {
    let stripped = url.strip_prefix("sqlite:")?;
    let path = stripped.split('?').next()?;
    let path = path.strip_prefix("//").unwrap_or(path);
    if path == ":memory:" {
        return None;
    }
    PathBuf::from(path).parent().map(|p| p.to_path_buf())
}
