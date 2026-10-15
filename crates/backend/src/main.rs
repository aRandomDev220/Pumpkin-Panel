mod audit;
mod backup_service;
mod auth;
mod config;
mod config_profile;
mod db;
mod error;
mod file_service;
mod health;
mod metrics;
mod readiness;
mod scheduler;
mod routes_admin;
mod routes_auth;
mod routes_backup;
mod routes_config;
mod routes_files;
mod routes_servers;
mod runtime;
mod task_runner;
#[cfg(test)]
mod tests_concurrency;
#[cfg(test)]
mod tests_docker;
#[cfg(test)]
mod tests_phase2;
#[cfg(test)]
mod tests_phase3;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tower_http::{limit::RequestBodyLimitLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use auth::Permission;
use runtime::{DockerAdapter, RuntimeAdapter};
use task_runner::RunnerHandle;

/// Shared application state (all cheaply cloneable handles).
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub adapter: Option<Arc<DockerAdapter>>,
    pub runner: Option<RunnerHandle>,
    pub server_root: PathBuf,
    pub op_locks: Arc<Mutex<HashSet<String>>>,
}

impl AppState {
    pub fn for_tests(pool: SqlitePool, server_root: PathBuf) -> Self {
        Self {
            pool,
            adapter: None,
            runner: None,
            server_root,
            op_locks: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

/// Assemble the full API router. Public routes first; everything else sits
/// behind `auth_middleware` + per-route permission gates.
pub fn build_router(state: AppState) -> Router {
    let pool = state.pool.clone();

    let protected = Router::new()
        .route("/api/v1/auth/logout", post(routes_auth::logout))
        .route("/api/v1/auth/me", get(routes_auth::me))
        .route(
            "/api/v1/auth/change-password",
            post(routes_auth::change_password),
        )
        .route(
            "/api/v1/servers",
            get(routes_servers::list_servers).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers",
            post(routes_servers::create_server).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerCreate),
            )),
        )
        .route(
            "/api/v1/servers/:id",
            get(routes_servers::get_server).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id",
            delete(routes_servers::delete_server).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerDelete),
            )),
        )
        .route(
            "/api/v1/servers/:id/:op",
            post(routes_servers::lifecycle).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerLifecycle),
            )),
        )
        .route(
            "/api/v1/servers/:id/status",
            get(routes_servers::server_status).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/logs",
            get(routes_servers::server_logs).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/tasks",
            get(routes_servers::server_tasks).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/metrics/summary",
            get(metrics::metrics_summary).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/capabilities",
            get(routes_servers::server_capabilities).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/tasks/:id",
            get(routes_admin::get_task).route_layer(middleware::from_fn(auth::require_permission(
                Permission::ServerRead,
            ))),
        )
        .route(
            "/api/v1/tasks/:id/cancel",
            post(routes_admin::cancel_task).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerLifecycle),
            )),
        )
        .route(
            "/api/v1/users",
            get(routes_admin::list_users).route_layer(middleware::from_fn(
                auth::require_permission(Permission::UsersManage),
            )),
        )
        .route(
            "/api/v1/users/:id",
            axum::routing::patch(routes_admin::update_user).route_layer(middleware::from_fn(
                auth::require_permission(Permission::UsersManage),
            )),
        )
        .route(
            "/api/v1/audit",
            get(routes_admin::list_audit).route_layer(middleware::from_fn(
                auth::require_permission(Permission::AuditRead),
            )),
        )
        .route(
            "/api/v1/admin/reconcile",
            post(routes_admin::trigger_reconcile).route_layer(middleware::from_fn(
                auth::require_permission(Permission::SettingsManage),
            )),
        )
        .route(
            "/api/v1/servers/:id/config",
            get(routes_config::get_config).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerConfigRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/config",
            put(routes_config::put_config).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerConfigWrite),
            )),
        )
        .route(
            "/api/v1/servers/:id/config/validate",
            post(routes_config::validate_config).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerConfigRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/config/revisions",
            get(routes_config::list_revisions).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerConfigRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/config/revert",
            post(routes_config::revert_config).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerConfigWrite),
            )),
        )
        .route(
            "/api/v1/servers/:id/files",
            get(routes_files::list_files).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/files/download",
            get(routes_files::download_file).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/files/directory",
            post(routes_files::create_directory).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesWrite),
            )),
        )
        .route(
            "/api/v1/servers/:id/files/rename",
            post(routes_files::rename_entry).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesWrite),
            )),
        )
        .route(
            "/api/v1/servers/:id/files",
            delete(routes_files::delete_entry).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesWrite),
            )),
        )
        .route(
            "/api/v1/servers/:id/files/export",
            get(routes_files::export_archive).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesRead),
            )),
        )
        .route_layer(middleware::from_fn_with_state(
            pool.clone(),
            auth::auth_middleware,
        ))
        // Default 5 MiB cap for all non-upload routes (upload router below
        // carries its own 1 GiB limit; public auth routes keep axum's
        // 2 MiB default, ample for login JSON).
        .layer(RequestBodyLimitLayer::new(5 * 1024 * 1024));

    // Uploads bypass the 5 MiB default body cap with their own 1 GiB limit
    // (enforced again as a running total inside the handler). Auth layer is
    // identical; the routers merge below.
    //
    // `DefaultBodyLimit::disable` is required here: axum 0.7's `Multipart`
    // extractor wraps the body in its own 2 MiB `Limited` unless this
    // extension is present, which would 400 every upload past 2 MiB no matter
    // what the tower layer allows. The tower 1 GiB layer + the handler's
    // running cap remain the real enforcement (streamed, never buffered).
    let protected_uploads = Router::new()
        .route(
            "/api/v1/servers/:id/files/upload",
            post(routes_files::upload_files).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesWrite),
            )),
        )
        .route(
            "/api/v1/servers/:id/files/import",
            post(routes_files::import_archive).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerFilesWrite),
            )),
        )
        .route_layer(middleware::from_fn_with_state(pool, auth::auth_middleware))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(1024 * 1024 * 1024));

    // Readiness reflects boot-time checks; Docker loss at runtime degrades
    // per-endpoint (503), never the process liveness probe.
    let readiness = health::ReadinessState {
        db_ok: true,
        docker_ok: state.adapter.is_some(),
    };
    Router::new()
        .route("/api/v1/health/live", get(health::liveness))
        .route(
            "/api/v1/health/ready",
            get(health::readiness).with_state(readiness),
        )
        .route("/api/v1/auth/setup-status", get(routes_auth::setup_status))
        .route("/api/v1/auth/setup", post(routes_auth::setup))
        .route("/api/v1/auth/login", post(routes_auth::login))
        .merge(protected)
        .merge(protected_uploads)
        .layer(TraceLayer::new_for_http())
        .fallback(static_handler)
        .with_state(state)
}

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

    // Recover tasks interrupted by a previous shutdown before accepting
    // work, so nothing stays `running` forever.
    match task_runner::requeue_interrupted(&pool).await {
        Ok(n) if n > 0 => tracing::warn!(recovered = n, "interrupted tasks failed safe"),
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "task recovery failed"),
    }

    // Docker is optional at boot: without a socket the manager still serves
    // inventory/auth and reports degraded runtime health.
    let adapter: Option<Arc<DockerAdapter>> = match DockerAdapter::new() {
        Ok(a) => {
            tracing::info!("docker daemon reachable");
            Some(Arc::new(a))
        }
        Err(e) => {
            tracing::warn!(error = %e, "docker unavailable; lifecycle ops will 503");
            None
        }
    };
    let runner = adapter
        .as_ref()
        .map(|a| task_runner::spawn_runner(pool.clone(), Arc::clone(a), 4));

    // Best-effort reconcile at startup: sync observed state, log drift.
    if let Some(a) = &adapter {
        match runtime::reconcile(&pool, a.as_ref()).await {
            Ok(items) => {
                for item in &items {
                    tracing::warn!(
                        server_id = %item.server_id,
                        kind = ?item.kind,
                        detail = %item.detail,
                        "startup drift"
                    );
                }
                sync_observed(&pool, a.as_ref()).await;
            }
            Err(e) => tracing::warn!(error = %e, "startup reconcile failed"),
        }
    }
    audit::record(
        &pool,
        None,
        "manager.startup",
        "manager",
        "boot",
        "ok",
        &audit::request_id(),
        serde_json::json!({ "docker": adapter.is_some() }),
    )
    .await;

    let server_root = settings.data_dir.clone();
    let state = AppState {
        pool,
        adapter,
        runner,
        server_root,
        op_locks: Arc::new(Mutex::new(HashSet::new())),
    };
    let docker_ok = state.adapter.is_some();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        port = settings.http_port,
        docker_ok,
        "pumpkin-manager starting"
    );

    let app = build_router(state);
    let addr = settings.bind_addr();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Sync `observed_state` from live container state (best-effort).
async fn sync_observed(pool: &SqlitePool, adapter: &DockerAdapter) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, container_id FROM servers WHERE deleted_at IS NULL AND container_id != ''",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, cid) in rows {
        let observed = match adapter.inspect(&cid).await {
            Ok(snap) => match snap.status.as_str() {
                "running" => "running",
                "created" => "created",
                "restarting" => "starting",
                "removing" => "stopping",
                _ => "exited",
            }
            .to_string(),
            Err(runtime::RuntimeError::NotFound(_)) => "missing".to_string(),
            Err(_) => continue,
        };
        let _ = sqlx::query("UPDATE servers SET observed_state = ? WHERE id = ?")
            .bind(&observed)
            .bind(&id)
            .execute(pool)
            .await;
    }
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
