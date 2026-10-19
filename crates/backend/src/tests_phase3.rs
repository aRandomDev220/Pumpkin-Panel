//! Phase 3 integration tests: readiness truthfulness, task history,
//! metrics summary, cooperative cancel. No Docker daemon (adapter `None`).

use axum_test::TestServer;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::{build_router, AppState};

async fn test_pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for file in [
        include_str!("../../../migrations/0001_init.sql"),
        include_str!("../../../migrations/0010_port_reservations.sql"),
        include_str!("../../../migrations/0002_sessions_audit.sql"),
        include_str!("../../../migrations/0007_update.sql"),
        include_str!("../../../migrations/0009_app_settings.sql"),
    ] {
        for stmt in file.split(";\n") {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            if !stmt.to_uppercase().contains("CREATE")
                && !stmt.to_uppercase().contains("INDEX")
                && !stmt.to_uppercase().contains("ALTER")
            {
                continue;
            }
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
    }
    pool
}

async fn test_server() -> (TestServer, SqlitePool) {
    let pool = test_pool().await;
    let root = std::env::temp_dir().join(format!("pm3-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let state = AppState::for_tests(pool.clone(), root);
    (TestServer::new(build_router(state)).unwrap(), pool)
}

async fn owner_token(server: &TestServer) -> String {
    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200);
    crate::auth::session_token_from_set_cookie(res.headers())
        .expect("pm_session cookie after setup")
}

async fn make_server(server: &TestServer, token: &str, name: &str, port: u16) -> String {
    let res = server
        .post("/api/v1/servers")
        .authorization_bearer(token)
        .json(&json!({"name": name, "game_port": port,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(res.status_code(), 201, "body: {}", res.text());
    res.json::<Value>()["server"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn status_reports_readiness_not_container_existence() {
    let (server, pool) = test_server().await;
    let t = owner_token(&server).await;
    let id = make_server(&server, &t, "ready1", 25565).await;

    // Without a Docker runtime, even a clean desired stop is unknown rather
    // than fabricated from stale database state.
    let status = server
        .get(&format!("/api/v1/servers/{id}/status"))
        .authorization_bearer(&t)
        .await
        .json::<Value>();
    assert_eq!(status["desired_state"], json!("stopped"));
    assert_eq!(status["readiness"]["state"], json!("unknown"));
    assert_eq!(status["readiness"]["ping"], json!(null));
    assert_eq!(status["runtime_state_stale"], json!(true));
    assert_ne!(status["readiness"]["state"], json!("online"));

    // A stale persisted running state must not become online while Docker is
    // unavailable, even if another process happens to answer the port.
    sqlx::query("UPDATE servers SET observed_state = 'running' WHERE id = ?")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let stale_status = server
        .get(&format!("/api/v1/servers/{id}/status"))
        .authorization_bearer(&t)
        .await
        .json::<Value>();
    assert_eq!(stale_status["readiness"]["state"], json!("unknown"));
    assert_eq!(stale_status["readiness"]["ping"], json!(null));
}

#[tokio::test]
async fn server_task_history() {
    let (server, pool) = test_server().await;
    let t = owner_token(&server).await;
    let id = make_server(&server, &t, "hist1", 25565).await;

    for task_type in ["start", "stop"] {
        sqlx::query("INSERT INTO tasks (id, type, server_id, state) VALUES (?, ?, ?, 'failed')")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(task_type)
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
    }

    let history = server
        .get(&format!("/api/v1/servers/{id}/tasks?limit=10"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(history.status_code(), 200, "body: {}", history.text());
    let body = history.json::<Value>();
    assert_eq!(body["total"], json!(2));
    assert_eq!(body["tasks"].as_array().unwrap().len(), 2);

    // Another server's tasks must not leak in.
    let other = make_server(&server, &t, "hist2", 25570).await;
    let empty = server
        .get(&format!("/api/v1/servers/{other}/tasks"))
        .authorization_bearer(&t)
        .await
        .json::<Value>();
    assert_eq!(empty["total"], json!(0));
}

#[tokio::test]
async fn metrics_summary_shape() {
    let (server, _) = test_server().await;
    let t = owner_token(&server).await;
    let _id = make_server(&server, &t, "met1", 25565).await;

    let res = server
        .get("/api/v1/metrics/summary")
        .authorization_bearer(&t)
        .await;
    assert_eq!(res.status_code(), 200, "body: {}", res.text());
    let body = res.json::<Value>();
    // Host facts are real numbers, not placeholders.
    assert!(body["host"]["memory_total_bytes"].as_i64().unwrap() > 0);
    assert!(body["host"]["disk_total_bytes"].as_i64().unwrap() > 0);
    // No runtime attached: per-server stats are explicit nulls, not zeros.
    let servers = body["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["cpu_percent"], json!(null));
    assert_eq!(servers[0]["memory_bytes"], json!(null));
}

#[tokio::test]
async fn cancel_requested_task_marks_cancelled_without_daemon() {
    let pool = test_pool().await;
    let task_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tasks (id, type, state) VALUES (?, 'stop', 'cancel_requested')")
        .bind(&task_id)
        .execute(&pool)
        .await
        .unwrap();
    // No adapter needed: cancellation is decided before any Docker call.
    // A stub that panics on use proves no runtime method runs.
    struct NeverCalled;
    #[allow(clippy::manual_async_fn)] // trait declares `-> impl Future`; async fn can't express it here
    impl crate::runtime::RuntimeAdapter for NeverCalled {
        fn create(
            &self,
            _spec: &crate::runtime::ValidatedServerSpec,
        ) -> impl std::future::Future<
            Output = Result<crate::runtime::RuntimeHandle, crate::runtime::RuntimeError>,
        > + Send {
            async { panic!("create must not run") }
        }
        fn start(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
        {
            async { panic!("start must not run") }
        }
        fn stop(
            &self,
            _container_id: &str,
            _timeout_secs: u32,
        ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
        {
            async { panic!("stop must not run") }
        }
        fn restart(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
        {
            async { panic!("restart must not run") }
        }
        fn remove(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
        {
            async { panic!("remove must not run") }
        }
        fn inspect(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<
            Output = Result<crate::runtime::RuntimeSnapshot, crate::runtime::RuntimeError>,
        > + Send {
            async { panic!("inspect must not run") }
        }
        fn logs(
            &self,
            _container_id: &str,
            _tail: usize,
        ) -> impl std::future::Future<Output = Result<Vec<String>, crate::runtime::RuntimeError>> + Send
        {
            async { panic!("logs must not run") }
        }
        fn stats(
            &self,
            _container_id: &str,
        ) -> impl std::future::Future<
            Output = Result<crate::runtime::RuntimeStats, crate::runtime::RuntimeError>,
        > + Send {
            async { panic!("stats must not run") }
        }
    }
    crate::task_runner::dispatch_task(&pool, &NeverCalled, &task_id)
        .await
        .expect("dispatch");
    let state: String = sqlx::query_scalar("SELECT state FROM tasks WHERE id = ?")
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "cancelled");
}
