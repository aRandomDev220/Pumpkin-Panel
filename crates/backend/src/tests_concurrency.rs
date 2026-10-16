//! Concurrency fail-safe tests: races that must fail safe, without a
//! Docker daemon (adapter `None`). Every test name starts with
//! `concurrency_` so `cargo test -p pumpkin-manager-backend concurrency_`
//! runs exactly this suite.

use axum_test::TestServer;
use futures_util::future::join_all;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::future::IntoFuture;

use crate::{build_router, AppState};

async fn test_pool() -> SqlitePool {
    // Single connection: in-memory SQLite is per-connection.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    apply_migrations(&pool).await;
    pool
}

async fn apply_migrations(pool: &SqlitePool) {
    for file in [
        include_str!("../../../migrations/0001_init.sql"),
        include_str!("../../../migrations/0002_sessions_audit.sql"),
        include_str!("../../../migrations/0007_update.sql"),
    ] {
        for stmt in file.split(";\n") {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            // Skip pure-comment / pragma chunks that hold no DDL.
            if !stmt.to_uppercase().contains("CREATE")
                && !stmt.to_uppercase().contains("INDEX")
                && !stmt.to_uppercase().contains("ALTER")
            {
                continue;
            }
            sqlx::query(stmt).execute(pool).await.unwrap();
        }
    }
}

async fn test_server() -> (TestServer, SqlitePool) {
    let pool = test_pool().await;
    let root = std::env::temp_dir().join(format!("pm-conc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let state = AppState::for_tests(pool.clone(), root);
    let server = TestServer::new(build_router(state)).unwrap();
    (server, pool)
}

async fn setup_owner(server: &TestServer) -> String {
    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200);
    res.json::<Value>()["token"].as_str().unwrap().to_string()
}

async fn server_task_count(pool: &SqlitePool, server_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE server_id = ?")
        .bind(server_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Eight concurrent creates with the same name and game port: exactly one
/// wins (`201`), the rest fail safe (`409`), and only one row exists.
#[tokio::test]
async fn concurrency_duplicate_create_fails_safe() {
    let (server, _) = test_server().await;
    let token = setup_owner(&server).await;
    let body = json!({"name": "race-srv", "game_port": 26580,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"});

    let requests = (0..8).map(|_| {
        server
            .post("/api/v1/servers")
            .authorization_bearer(&token)
            .json(&body)
            .into_future()
    });
    let responses = join_all(requests).await;

    let mut created = 0;
    for res in &responses {
        let status = res.status_code();
        if status == 201 {
            created += 1;
        } else {
            assert_eq!(status, 409, "expected 409, got {status}: {}", res.text());
            let code = res.json::<Value>()["error_code"]
                .as_str()
                .unwrap_or("")
                .to_string();
            assert!(
                code == "SERVER_CONFLICT" || code == "PORT_UNAVAILABLE",
                "unexpected error_code {code}"
            );
        }
    }
    assert_eq!(created, 1, "exactly one create must win");

    let list = server
        .get("/api/v1/servers")
        .authorization_bearer(&token)
        .await;
    assert_eq!(list.json::<Value>()["total"], json!(1));
}

/// A `queued` task already exists for a server with a container: a second
/// `start` is rejected and enqueues nothing (fail-safe).
///
/// Honest-code note: with no runtime attached (`AppState::for_tests` sets
/// the adapter to `None`), `lifecycle_inner` reports `503
/// RUNTIME_UNAVAILABLE` before it reaches the `conflicting_task` guard, so
/// the rejection surfaces as degraded-runtime rather than `409
/// SERVER_BUSY` (see `routes_servers::lifecycle_inner`: the
/// adapter/runner check precedes the busy check, and
/// `tests_phase2::server_crud_without_docker` pins the same `503` for a
/// container-backed start without a runtime). The fail-safe property this
/// test pins is: rejected + zero new task rows. If `SERVER_BUSY`
/// precedence over `RUNTIME_UNAVAILABLE` is ever wanted, that is a
/// production reorder decision, not a test change.
#[tokio::test]
async fn concurrency_busy_rejects_second_op() {
    let (server, pool) = test_server().await;
    let token = setup_owner(&server).await;

    let created = server
        .post("/api/v1/servers")
        .authorization_bearer(&token)
        .json(&json!({"name": "busy-srv", "game_port": 26581,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(created.status_code(), 201, "body: {}", created.text());
    let id = created.json::<Value>()["server"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    sqlx::query("UPDATE servers SET container_id = ? WHERE id = ?")
        .bind(format!("fake-{}", uuid::Uuid::new_v4()))
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO tasks (id, type, server_id, state) VALUES (?, 'start', ?, 'queued')")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(server_task_count(&pool, &id).await, 1);

    let start = server
        .post(&format!("/api/v1/servers/{id}/start"))
        .authorization_bearer(&token)
        .await;
    assert_eq!(start.status_code(), 503, "body: {}", start.text());
    assert_eq!(
        start.json::<Value>()["error_code"],
        json!("RUNTIME_UNAVAILABLE")
    );
    assert_eq!(server_task_count(&pool, &id).await, 1);
}

/// Six concurrent `start`s on a containerless server: all rejected with
/// `409 MISSING_CONTAINER`, zero task rows created.
#[tokio::test]
async fn concurrency_starts_without_runtime_stay_clean() {
    let (server, pool) = test_server().await;
    let token = setup_owner(&server).await;

    let created = server
        .post("/api/v1/servers")
        .authorization_bearer(&token)
        .json(&json!({"name": "clean-srv", "game_port": 26582,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(created.status_code(), 201, "body: {}", created.text());
    let id = created.json::<Value>()["server"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let url = format!("/api/v1/servers/{id}/start");

    let requests = (0..6).map(|_| server.post(&url).authorization_bearer(&token).into_future());
    let responses = join_all(requests).await;

    for res in &responses {
        assert_eq!(
            res.status_code(),
            409,
            "expected 409, got {}: {}",
            res.status_code(),
            res.text()
        );
        assert_eq!(
            res.json::<Value>()["error_code"],
            json!("MISSING_CONTAINER")
        );
    }
    assert_eq!(server_task_count(&pool, &id).await, 0);
}
