mod audit;
mod auth;
mod backup_service;
mod config;
mod config_profile;
mod db;
mod file_service;
mod health;
mod metrics;
mod metrics_collector;
mod op_lock;
mod ports;
mod ratelimit;
mod readiness;
mod reconcile;
mod routes_admin;
mod routes_auth;
mod routes_backup;
mod routes_config;
mod routes_files;
mod routes_metrics;
mod routes_servers;
mod runtime;
mod scheduler;
mod task_runner;
#[cfg(test)]
mod tests_concurrency;
#[cfg(test)]
mod tests_docker;
#[cfg(test)]
mod tests_phase2;
#[cfg(test)]
mod tests_phase3;
mod update_service;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    middleware,
    routing::{delete, get, patch, post, put},
    Router,
};
use sqlx::{Connection as _, SqlitePool};
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
    /// Single source of truth for backup archive storage (from
    /// `settings.backup_dir`, which itself honors `PUMPKIN_MANAGER_BACKUP_DIR`).
    /// All backup routes/workers must use this, never a derived heuristic.
    pub backup_dir: PathBuf,
    /// Root of the bundled SPA build. Requests that miss the API are served
    /// from here (with traversal rejected). Production default is `/app/web`;
    /// tests point it at a temp dir.
    pub web_root: PathBuf,
    pub op_locks: Arc<Mutex<HashSet<String>>>,
    /// Session lifetime in seconds (cookie `Max-Age` + session expiry),
    /// derived from `settings.session_ttl_minutes`.
    pub session_ttl_secs: i64,
    /// When false (env `PUMPKIN_INSECURE_COOKIES=1`), cookies omit `Secure` and
    /// HSTS is suppressed. Intended only for local HTTP development.
    pub secure_cookies: bool,
    /// In-process brute-force limiter for `/auth/login` and `/auth/setup`.
    pub rate_limiter: Arc<std::sync::Mutex<ratelimit::RateLimiter>>,
}

impl AppState {
    pub fn for_tests(pool: SqlitePool, server_root: PathBuf) -> Self {
        Self {
            pool,
            adapter: None,
            runner: None,
            backup_dir: server_root.join("backups"),
            web_root: server_root.join("web"),
            server_root,
            op_locks: Arc::new(Mutex::new(HashSet::new())),
            session_ttl_secs: 12 * 60 * 60,
            secure_cookies: true,
            rate_limiter: new_rate_limiter(),
        }
    }

    /// Test-only: exercise the insecure-cookie development mode without
    /// mutating the process environment (which is shared across tests).
    #[cfg(test)]
    pub fn with_insecure_cookies(mut self) -> Self {
        self.secure_cookies = false;
        self
    }
}

fn new_rate_limiter() -> Arc<std::sync::Mutex<ratelimit::RateLimiter>> {
    Arc::new(std::sync::Mutex::new(ratelimit::RateLimiter::new(
        ratelimit::AUTH_ATTEMPTS,
        ratelimit::AUTH_WINDOW,
    )))
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
            "/api/v1/servers/:id/settings",
            get(routes_servers::server_settings).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/settings",
            patch(routes_servers::update_server_settings).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerConfigWrite),
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
            "/api/v1/servers/:id/metrics",
            get(routes_metrics::server_metrics).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/alerts",
            get(routes_metrics::list_alerts).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/scheduled-tasks",
            get(scheduler::list_scheduled_tasks).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerLifecycle),
            )),
        )
        .route(
            "/api/v1/servers/:id/scheduled-tasks",
            post(scheduler::create_scheduled_task).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerLifecycle),
            )),
        )
        .route(
            "/api/v1/scheduled-tasks/:id",
            delete(scheduler::delete_scheduled_task).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerLifecycle),
            )),
        )
        .route(
            "/api/v1/admin/diagnostics",
            get(routes_admin::export_diagnostics).route_layer(middleware::from_fn(
                auth::require_permission(Permission::SettingsManage),
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
            "/api/v1/servers/:id/update/preview",
            post(update_service::preview_image_update).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerUpdate),
            )),
        )
        .route(
            "/api/v1/servers/:id/update",
            post(update_service::update_server).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerUpdate),
            )),
        )
        .route(
            "/api/v1/servers/:id/rollback",
            post(update_service::rollback_server).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerUpdate),
            )),
        )
        .route(
            "/api/v1/users",
            post(routes_admin::create_user).route_layer(middleware::from_fn(
                auth::require_permission(Permission::UsersManage),
            )),
        )
        .route(
            "/api/v1/users/:id",
            delete(routes_admin::delete_user).route_layer(middleware::from_fn(
                auth::require_permission(Permission::UsersManage),
            )),
        )
        .route(
            "/api/v1/users/:id/reset-password",
            post(routes_admin::reset_user_password).route_layer(middleware::from_fn(
                auth::require_permission(Permission::UsersManage),
            )),
        )
        .route(
            "/api/v1/servers/:id/backups",
            get(routes_backup::list_backups).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/backups",
            post(routes_backup::create_backup).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerBackupCreate),
            )),
        )
        .route(
            "/api/v1/backups/:id",
            get(routes_backup::get_backup).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/backups/:id/download",
            get(routes_backup::download_backup).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/backups/:id",
            delete(routes_backup::delete_backup).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerBackupCreate),
            )),
        )
        .route(
            "/api/v1/backups/:id/restore",
            post(routes_backup::restore_backup).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerBackupRestore),
            )),
        )
        .route(
            "/api/v1/servers/:id/backup-policies",
            get(scheduler::list_policies).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/servers/:id/backup-policies",
            post(scheduler::create_policy).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerBackupCreate),
            )),
        )
        .route(
            "/api/v1/servers/:id/backup-policies/preview",
            post(scheduler::retention_preview).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerRead),
            )),
        )
        .route(
            "/api/v1/backup-policies/:id",
            axum::routing::patch(scheduler::update_policy).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerBackupCreate),
            )),
        )
        .route(
            "/api/v1/backup-policies/:id",
            delete(scheduler::delete_policy).route_layer(middleware::from_fn(
                auth::require_permission(Permission::ServerBackupCreate),
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
        .route(
            "/api/v1/admin/database/backup",
            get(routes_admin::backup_database).route_layer(middleware::from_fn(
                auth::require_permission(Permission::SettingsManage),
            )),
        )
        .route(
            "/api/v1/admin/database/restore",
            post(routes_admin::restore_database).route_layer(middleware::from_fn(
                auth::require_permission(Permission::SettingsManage),
            )),
        )
        .route_layer(middleware::from_fn_with_state(pool, auth::auth_middleware))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(1024 * 1024 * 1024));

    // Public auth routes carry no session yet, so they enforce a same-origin
    // `Origin` check plus rate limiting in-handler. Bodies are tiny (username +
    // password), so cap them well below the default to blunt body-based DoS.
    let public_auth = Router::new()
        .route("/api/v1/auth/setup-status", get(routes_auth::setup_status))
        .route("/api/v1/auth/setup", post(routes_auth::setup))
        .route("/api/v1/auth/login", post(routes_auth::login))
        .layer(RequestBodyLimitLayer::new(64 * 1024));

    // `/live` is process-only. `/ready` probes the database and Docker on
    // every call so orchestration sees a manager that lost either.
    Router::new()
        .route("/api/v1/health/live", get(health::liveness))
        .route("/api/v1/health/ready", get(health::readiness))
        .merge(public_auth)
        .merge(protected)
        .merge(protected_uploads)
        // Register the fallback BEFORE the layers: `Router::layer` only wraps
        // endpoints present at call time, and a later `.fallback()` would
        // replace the layered default endpoint — leaving static/SPA responses
        // (index.html, JS, CSS) without security headers.
        .fallback(static_handler)
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ))
        .with_state(state)
}

/// Adds baseline security headers to every response (including the SPA
/// fallback). HSTS is only emitted when cookies are secure (i.e. not in the
/// explicit local-HTTP development mode).
async fn security_headers(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> axum::response::Response {
    use axum::http::{header, HeaderName, HeaderValue};

    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; \
             base-uri 'self'; form-action 'self'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
    );
    if state.secure_cookies {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    res
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

    // Apply a staged manager-DB restore (from POST /admin/database/restore)
    // BEFORE opening the pool: swap the validated file into place, keeping
    // a timestamped copy of the replaced database alongside it.
    if let Some(db_file) = db_file_path(&settings.database_url) {
        let pending = routes_admin::pending_restore_path(&db_file);
        if pending.is_file() {
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
            let aside = PathBuf::from(format!("{}.pre-restore-{stamp}", db_file.display()));
            if db_file.is_file() {
                // Fold committed WAL frames into the main file, then remove
                // the sidecars. Renaming only the main file would let the old
                // WAL replay into the restored database.
                if let Ok(mut conn) = sqlx::SqliteConnection::connect(&settings.database_url).await
                {
                    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                        .execute(&mut conn)
                        .await;
                    let _ = conn.close().await;
                }
                std::fs::rename(&db_file, &aside)?;
                for suffix in ["-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", db_file.display()));
                }
                tracing::warn!(aside = %aside.display(), "manager DB replaced aside");
            }
            std::fs::rename(&pending, &db_file)?;
            if let Some(parent) = db_file.parent() {
                #[cfg(unix)]
                if let Ok(dir) = std::fs::File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            tracing::warn!("staged manager DB restore applied; re-validating on boot");
        }
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
    let op_locks: crate::op_lock::OpLocks = Arc::new(Mutex::new(HashSet::new()));
    let runner = adapter
        .as_ref()
        .map(|a| task_runner::spawn_runner(pool.clone(), Arc::clone(a), 4, Arc::clone(&op_locks)));

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
                if let Some(runner) = &runner {
                    schedule_auto_start_servers(&pool, runner).await;
                }
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

    // Web root: bundled SPA build, overridable for non-images deployments.
    let web_root = std::env::var_os("PUMPKIN_MANAGER_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/app/web"));
    let server_root = settings.data_dir.clone();
    // Backup storage has one source of truth: `settings.backup_dir` (env
    // `PUMPKIN_MANAGER_BACKUP_DIR` feeds `Settings`, never the routes).
    // Boot cleanup, the scheduler tick, and `AppState` all share it.
    let backup_root = settings.backup_dir.clone();
    if let Err(e) = std::fs::create_dir_all(&backup_root) {
        tracing::warn!(path = %backup_root.display(), error = %e, "cannot create backup dir");
    }
    routes_backup::startup_cleanup(&pool, &backup_root).await;
    routes_admin::cleanup_temp_snapshots();
    // Single-node scheduler: one loop per manager process (no second replica
    // may run against the same DB — see spec §19.2). A panic restarts the
    // iteration instead of silently disabling every schedule for the process
    // lifetime.
    {
        let pool = pool.clone();
        let adapter = adapter.clone();
        let backup_root = backup_root.clone();
        let reconcile_runner = runner.clone();
        tokio::spawn(async move {
            let mut reconciler = reconcile::Reconciler::default();
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let tick = std::panic::AssertUnwindSafe(async {
                    scheduler::tick_once(
                        &pool,
                        adapter.as_deref(),
                        &backup_root,
                        reconcile_runner.as_ref(),
                    )
                    .await;
                    reconciler
                        .tick(&pool, adapter.as_deref(), reconcile_runner.as_ref())
                        .await;
                });
                if futures_util::FutureExt::catch_unwind(tick).await.is_err() {
                    tracing::error!("scheduler tick panicked; continuing");
                }
                if let Err(e) = db::prune_ephemeral(&pool).await {
                    tracing::warn!(error = %e, "ephemeral retention failed");
                }
            }
        });
    }
    // Metrics are independent: a slow probe must never delay schedules.
    {
        let pool = pool.clone();
        let adapter = adapter.clone();
        let metrics_secs = settings.metrics_interval_seconds.max(10);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(metrics_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let tick = std::panic::AssertUnwindSafe(metrics_collector::sample_all(
                    &pool,
                    adapter.as_deref(),
                ));
                if futures_util::FutureExt::catch_unwind(tick).await.is_err() {
                    tracing::error!("metrics tick panicked; continuing");
                }
            }
        });
    }
    let insecure_cookies = std::env::var("PUMPKIN_INSECURE_COOKIES").as_deref() == Ok("1");
    if insecure_cookies {
        tracing::warn!(
            "PUMPKIN_INSECURE_COOKIES=1: session cookies omit Secure and HSTS is disabled; \
             use only for local HTTP development"
        );
    }
    let state = AppState {
        pool: pool.clone(),
        adapter,
        runner,
        server_root,
        backup_dir: backup_root,
        web_root,
        op_locks,
        session_ttl_secs: settings.session_ttl_secs(),
        secure_cookies: !insecure_cookies,
        rate_limiter: new_rate_limiter(),
    };
    let docker_ok = state.adapter.is_some();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        port = settings.http_port,
        docker_ok,
        "pumpkin-manager starting"
    );

    let shutdown_pool = pool.clone();
    let app = build_router(state);
    let addr = settings.bind_addr();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    // Give in-flight tasks a bounded window to reach a safe point before the
    // process exits; log honestly if some are still running.
    tracing::warn!("http server stopped; waiting up to 30s for in-flight tasks");
    for _ in 0..30 {
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks WHERE state IN ('queued','running','cancel_requested')",
        )
        .fetch_one(&shutdown_pool)
        .await
        .unwrap_or(0);
        if active == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    audit::record(
        &shutdown_pool,
        None,
        "manager.shutdown",
        "manager",
        "shutdown",
        "ok",
        &audit::request_id(),
        serde_json::json!({}),
    )
    .await;
    Ok(())
}

/// Resolve on SIGINT/SIGTERM so axum can drain HTTP connections.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::warn!("shutdown signal received; draining connections");
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

/// Queue opt-in server starts after the first Docker reconciliation. This is
/// deliberately manager-owned rather than a Docker restart policy: an owner
/// who stops a world stays in control until the next boot, and every start is
/// still represented by a persisted task.
async fn schedule_auto_start_servers(pool: &SqlitePool, runner: &task_runner::RunnerHandle) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT s.id, s.observed_state
         FROM servers s
         JOIN server_settings ss ON ss.server_id = s.id
         WHERE s.deleted_at IS NULL AND ss.auto_start = 1 AND s.container_id != ''",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for (server_id, observed_state) in rows {
        if !should_queue_auto_start(&observed_state) {
            tracing::debug!(server_id = %server_id, observed_state = %observed_state, "auto-start skipped for current runtime state");
            continue;
        }
        let in_flight: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks
             WHERE server_id = ? AND state IN ('queued', 'running', 'cancel_requested')",
        )
        .bind(&server_id)
        .fetch_one(pool)
        .await
        .unwrap_or(1);
        if in_flight > 0 {
            continue;
        }
        let _ = sqlx::query(
            "UPDATE servers SET desired_state = 'running',
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?",
        )
        .bind(&server_id)
        .execute(pool)
        .await;
        match task_runner::enqueue_and_submit(pool, runner, "start", Some(&server_id), None).await {
            Ok(task) => {
                tracing::info!(server_id = %server_id, task_id = %task.id, "queued auto-start")
            }
            Err(e) => {
                tracing::warn!(server_id = %server_id, error = %e, "auto-start could not be queued");
                let _ = sqlx::query(
                    "UPDATE servers SET desired_state = 'stopped',
                            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE id = ?",
                )
                .bind(&server_id)
                .execute(pool)
                .await;
            }
        }
    }
}

fn should_queue_auto_start(observed_state: &str) -> bool {
    matches!(observed_state, "created" | "exited" | "stopped")
}

/// Serves the Vite build (`apps/web/dist`) when bundled in the single
/// Docker image. Unknown non-API paths fall back to `index.html` for
/// client-side routing; traversal attempts are rejected with 404.
async fn static_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    let dist = state.web_root.clone();
    serve_static(&dist, req.uri().path())
}

fn serve_static(dist: &std::path::Path, raw_path: &str) -> axum::response::Response {
    use axum::http::StatusCode;

    // Never shadow API routes.
    if raw_path.starts_with("/api/") {
        return (StatusCode::NOT_FOUND, "{\"error_code\":\"NOT_FOUND\"}").into_response();
    }

    // Percent-decode once, then reject any `..` component. `Path::starts_with`
    // does NOT resolve `..`, so it must never be the containment check.
    let Some(decoded) = percent_decode(raw_path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if decoded.contains('\0') || has_parent_component(&decoded) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    let candidate = if decoded == "/" {
        dist.join("index.html")
    } else {
        dist.join(decoded.trim_start_matches('/'))
    };
    // Belt-and-braces containment: canonicalize both sides (resolves `..` and
    // symlinks) and require the file to resolve inside the web root.
    if let (Ok(root), Ok(file)) = (dist.canonicalize(), candidate.canonicalize()) {
        if file.starts_with(&root) && file.is_file() {
            let body = std::fs::read(&file).unwrap_or_default();
            let ct = mime_for(&file);
            return ([("content-type", ct)], body).into_response();
        }
    }

    // SPA fallback: unknown, non-traversing routes get index.html.
    let index = dist.join("index.html");
    if index.is_file() {
        let body = std::fs::read(&index).unwrap_or_default();
        return ([("content-type", "text/html")], body).into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

/// True if any `/`-separated component is exactly `..`.
fn has_parent_component(path: &str) -> bool {
    path.split('/').any(|c| c == "..")
}

/// Minimal one-pass percent-decoder for a request path. Returns `None` on a
/// malformed escape or invalid UTF-8. `%2f`/`%2e` are decoded so an encoded
/// traversal cannot slip past [`has_parent_component`].
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_value(bytes[i + 1])?;
            let lo = hex_value(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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

fn db_file_path(url: &str) -> Option<PathBuf> {
    let stripped = url.strip_prefix("sqlite:")?;
    let path = stripped.split('?').next()?;
    let path = path.strip_prefix("//").unwrap_or(path);
    if path == ":memory:" {
        return None;
    }
    Some(PathBuf::from(path))
}

fn db_path_parent(url: &str) -> Option<PathBuf> {
    let stripped = url.strip_prefix("sqlite:")?;
    let path = stripped.split('?').next()?;
    let path = path.strip_prefix("//").unwrap_or(path);
    if path == ":memory:" {
        return None;
    }
    PathBuf::from(path).parent().map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use crate::{should_queue_auto_start, AppState};
    use sqlx::SqlitePool;

    async fn empty_pool() -> SqlitePool {
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool")
    }

    /// Unauthenticated path traversal in the SPA fallback must 404 and must
    /// never read a file outside the web root.
    #[tokio::test]
    async fn spa_fallback_rejects_traversal() {
        let root = std::env::temp_dir().join(format!("pm-spa-{}", uuid::Uuid::new_v4()));
        let web = root.join("web");
        std::fs::create_dir_all(&web).expect("web root");
        std::fs::write(web.join("index.html"), "<html>SPA-INDEX-MARKER</html>").expect("index");
        let secret = "TOP-SECRET-OUTSIDE-WEB-ROOT";
        std::fs::write(root.join("Cargo.toml"), secret).expect("secret file");

        let state = AppState::for_tests(empty_pool().await, root.clone());
        let app = crate::build_router(state);

        // `axum_test` normalizes `..` via the `url` crate before dispatch, so
        // drive the real router directly to send the path verbatim (the
        // production case: `curl --path-as-is` / hyper do not normalize).
        use tower::ServiceExt as _;
        for path in ["/../Cargo.toml", "/..%2fCargo.toml", "/%2e%2e/Cargo.toml"] {
            let req = axum::http::Request::builder()
                .uri(path)
                .body(axum::body::Body::empty())
                .expect("request");
            let res = app.clone().oneshot(req).await.expect("router call");
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
                .await
                .expect("body");
            let body = String::from_utf8_lossy(&bytes);
            assert_eq!(status, 404, "path {path} body: {body}");
            assert!(
                !body.contains(secret),
                "path {path} leaked file contents outside the web root"
            );
        }

        // The SPA is still served for genuine client-side routes.
        let server = axum_test::TestServer::new(app).expect("server");
        let res = server.get("/servers/abc/overview").await;
        assert_eq!(res.status_code(), 200);
        assert!(res.text().contains("SPA-INDEX-MARKER"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_start_skips_running_and_missing_containers() {
        assert!(should_queue_auto_start("created"));
        assert!(should_queue_auto_start("exited"));
        assert!(should_queue_auto_start("stopped"));
        assert!(!should_queue_auto_start("running"));
        assert!(!should_queue_auto_start("missing"));
        assert!(!should_queue_auto_start("unknown"));
    }
}
