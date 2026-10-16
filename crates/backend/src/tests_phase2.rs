//! Phase 2 integration tests: auth flow, RBAC matrix, server inventory,
//! lifecycle guards, tasks, users, audit — all without a Docker daemon
//! (adapter `None`), proving the API degrades honestly.

use axum_test::TestServer;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::{auth, build_router, AppState};

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
    let root = std::env::temp_dir().join(format!("pm-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let state = AppState::for_tests(pool.clone(), root);
    let server = TestServer::new(build_router(state)).unwrap();
    (server, pool)
}

fn token(body: &Value) -> String {
    body["token"].as_str().unwrap().to_string()
}

async fn setup_owner(server: &TestServer) -> String {
    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200);
    token(&res.json())
}

async fn make_user(pool: &SqlitePool, username: &str, role: &str, password: &str) {
    let hash = auth::hash_password(password).unwrap();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, is_active)
         VALUES (?, ?, ?, ?, 1)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(username)
    .bind(&hash)
    .bind(role)
    .execute(pool)
    .await
    .unwrap();
}

async fn login(server: &TestServer, username: &str, password: &str) -> String {
    let res = server
        .post("/api/v1/auth/login")
        .json(&json!({"username": username, "password": password}))
        .await;
    assert_eq!(res.status_code(), 200, "login failed: {:?}", res.text());
    token(&res.json())
}

#[tokio::test]
async fn setup_flow() {
    let (server, _) = test_server().await;
    let status = server.get("/api/v1/auth/setup-status").await;
    assert_eq!(status.json::<Value>()["needs_setup"], json!(true));

    let owner_token = setup_owner(&server).await;
    assert!(!owner_token.is_empty());

    let status = server.get("/api/v1/auth/setup-status").await;
    assert_eq!(status.json::<Value>()["needs_setup"], json!(false));

    // Second setup is rejected, not silently duplicated.
    let again = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner2", "password": "supersecret1"}))
        .await;
    assert_eq!(again.status_code(), 409);

    // Me works with the fresh token.
    let me = server
        .get("/api/v1/auth/me")
        .authorization_bearer(&owner_token)
        .await;
    let me_json = me.json::<Value>();
    assert_eq!(me_json["username"], json!("owner"));
    assert_eq!(me_json["role"], json!("owner"));
}

#[tokio::test]
async fn unauthenticated_is_rejected() {
    let (server, _) = test_server().await;
    assert_eq!(server.get("/api/v1/servers").await.status_code(), 401);
    let res = server
        .post("/api/v1/servers")
        .json(&json!({"name": "x", "game_port": 25565,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(res.status_code(), 401);
    assert_eq!(server.get("/api/v1/auth/me").await.status_code(), 401);
}

#[tokio::test]
async fn server_crud_without_docker() {
    let (server, pool) = test_server().await;
    let t = setup_owner(&server).await;

    let list = server.get("/api/v1/servers").authorization_bearer(&t).await;
    assert_eq!(list.json::<Value>()["total"], json!(0));

    // Create without a runtime: row persists, no container, digest warning.
    let created = server
        .post("/api/v1/servers")
        .authorization_bearer(&t)
        .json(&json!({"name": "survival", "game_port": 25565,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(created.status_code(), 201, "body: {}", created.text());
    let body = created.json::<Value>();
    let id = body["server"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["server"]["observed_state"], json!("created"));
    assert!(!body["warnings"].as_array().unwrap().is_empty());

    // Port (and adjacent bedrock port) cannot be double-allocated.
    for port in [25565u16, 25566u16] {
        let dup = server
            .post("/api/v1/servers")
            .authorization_bearer(&t)
            .json(&json!({"name": format!("other-{port}"), "game_port": port,
                          "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
            .await;
        assert_eq!(dup.status_code(), 409, "port {port}: {}", dup.text());
    }

    // Non-allowlisted images are rejected, not passed to Docker.
    let evil = server
        .post("/api/v1/servers")
        .authorization_bearer(&t)
        .json(&json!({"name": "evil", "game_port": 25570,
                      "image_reference": "docker.io/library/ubuntu:latest"}))
        .await;
    assert_eq!(evil.status_code(), 422);

    // Lifecycle on a containerless row: honest 409, not a silent no-op.
    let start = server
        .post(&format!("/api/v1/servers/{id}/start"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(start.status_code(), 409);
    assert_eq!(
        start.json::<Value>()["error_code"],
        json!("MISSING_CONTAINER")
    );

    // Lifecycle with a container but no runtime: honest 503.
    let cid = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE servers SET container_id = ? WHERE id = ?")
        .bind(&cid)
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let start = server
        .post(&format!("/api/v1/servers/{id}/start"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(start.status_code(), 503);
    assert_eq!(
        start.json::<Value>()["error_code"],
        json!("RUNTIME_UNAVAILABLE")
    );

    // Status reports DB truth + degraded flag.
    let status = server
        .get(&format!("/api/v1/servers/{id}/status"))
        .authorization_bearer(&t)
        .await;
    let status_json = status.json::<Value>();
    assert_eq!(status_json["desired_state"], json!("stopped"));
    assert_eq!(status_json["docker_available"], json!(false));

    // Capabilities never claim the unverified.
    let caps = server
        .get(&format!("/api/v1/servers/{id}/capabilities"))
        .authorization_bearer(&t)
        .await;
    let caps_json = caps.json::<Value>();
    assert_eq!(
        caps_json["capabilities"]["rcon"]["status"],
        json!("unknown")
    );

    // Delete with data drop; gone afterwards. (Clear the fake container
    // id first: with a container but no runtime, delete honestly 503s.)
    sqlx::query("UPDATE servers SET container_id = '' WHERE id = ?")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let del = server
        .delete(&format!("/api/v1/servers/{id}?retain_data=false"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(del.status_code(), 204);
    let gone = server
        .get(&format!("/api/v1/servers/{id}"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(gone.status_code(), 404);
}

#[tokio::test]
async fn rbac_matrix() {
    let (server, pool) = test_server().await;
    let owner = setup_owner(&server).await;
    make_user(&pool, "viewer1", "viewer", "viewersecret1").await;
    make_user(&pool, "admin1", "admin", "adminsecret1").await;
    let viewer = login(&server, "viewer1", "viewersecret1").await;
    let admin = login(&server, "admin1", "adminsecret1").await;

    // Viewer reads, cannot mutate.
    let list = server
        .get("/api/v1/servers")
        .authorization_bearer(&viewer)
        .await;
    assert_eq!(list.status_code(), 200);
    let denied = server
        .post("/api/v1/servers")
        .authorization_bearer(&viewer)
        .json(&json!({"name": "nope", "game_port": 25580,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(denied.status_code(), 403);

    // Admin creates; viewer cannot even delete.
    let created = server
        .post("/api/v1/servers")
        .authorization_bearer(&admin)
        .json(&json!({"name": "adminsrv", "game_port": 25581,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(created.status_code(), 201);
    let id = created.json::<Value>()["server"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let vdel = server
        .delete(&format!("/api/v1/servers/{id}"))
        .authorization_bearer(&viewer)
        .await;
    assert_eq!(vdel.status_code(), 403);

    // Users/audit are owner(+admin for audit)-gated.
    assert_eq!(
        server
            .get("/api/v1/users")
            .authorization_bearer(&viewer)
            .await
            .status_code(),
        403
    );
    assert_eq!(
        server
            .get("/api/v1/users")
            .authorization_bearer(&admin)
            .await
            .status_code(),
        403
    );
    let users = server
        .get("/api/v1/users")
        .authorization_bearer(&owner)
        .await;
    assert_eq!(users.status_code(), 200);
    assert!(users.json::<Value>()["users"].as_array().unwrap().len() >= 3);

    assert_eq!(
        server
            .get("/api/v1/audit")
            .authorization_bearer(&viewer)
            .await
            .status_code(),
        403
    );
    let audit = server
        .get("/api/v1/audit")
        .authorization_bearer(&admin)
        .await;
    assert_eq!(audit.status_code(), 200);
}

#[tokio::test]
async fn tasks_and_reconcile() {
    let (server, pool) = test_server().await;
    let owner = setup_owner(&server).await;

    // Unknown task: 404, not 500.
    assert_eq!(
        server
            .get("/api/v1/tasks/does-not-exist")
            .authorization_bearer(&owner)
            .await
            .status_code(),
        404
    );

    // A queued task can be cancelled; unknown cancel is 404.
    let task_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tasks (id, type, state) VALUES (?, 'start', 'queued')")
        .bind(&task_id)
        .execute(&pool)
        .await
        .unwrap();
    let cancel = server
        .post(&format!("/api/v1/tasks/{task_id}/cancel"))
        .authorization_bearer(&owner)
        .await;
    assert_eq!(cancel.status_code(), 200);
    assert_eq!(
        server
            .get(&format!("/api/v1/tasks/{task_id}"))
            .authorization_bearer(&owner)
            .await
            .json::<Value>()["state"],
        json!("cancelled")
    );
    assert_eq!(
        server
            .post("/api/v1/tasks/does-not-exist/cancel")
            .authorization_bearer(&owner)
            .await
            .status_code(),
        404
    );

    // Reconcile without a runtime is an honest 503.
    let rec = server
        .post("/api/v1/admin/reconcile")
        .authorization_bearer(&owner)
        .await;
    assert_eq!(rec.status_code(), 503);
}

#[tokio::test]
async fn last_owner_guard() {
    let (server, _) = test_server().await;
    let owner = setup_owner(&server).await;
    let me = server
        .get("/api/v1/auth/me")
        .authorization_bearer(&owner)
        .await
        .json::<Value>();
    let id = me["id"].as_str().unwrap().to_string();
    let res = server
        .patch(&format!("/api/v1/users/{id}"))
        .authorization_bearer(&owner)
        .json(&json!({"role": "viewer"}))
        .await;
    assert_eq!(res.status_code(), 409);
}
