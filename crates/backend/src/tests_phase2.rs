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

fn session_token(res: &axum_test::TestResponse) -> String {
    auth::session_token_from_set_cookie(res.headers()).expect("pm_session cookie")
}

async fn setup_owner(server: &TestServer) -> String {
    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200);
    session_token(&res)
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
    session_token(&res)
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

    // Resource settings round-trip and remain bounded by the runtime policy.
    let settings = server
        .get(&format!("/api/v1/servers/{id}/settings"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(settings.status_code(), 200);
    assert_eq!(
        settings.json::<Value>()["settings"]["memory_limit_bytes"],
        json!(2 * 1024 * 1024 * 1024u64)
    );
    let updated_settings = server
        .patch(&format!("/api/v1/servers/{id}/settings"))
        .authorization_bearer(&t)
        .json(&json!({
            "memory_limit_bytes": 4 * 1024 * 1024 * 1024u64,
            "cpu_limit": 3.5,
            "auto_start": true,
        }))
        .await;
    assert_eq!(updated_settings.status_code(), 200);
    let updated_json = updated_settings.json::<Value>();
    assert_eq!(updated_json["settings"]["cpu_limit"], json!(3.5));
    assert_eq!(updated_json["settings"]["auto_start"], json!(true));
    assert_eq!(updated_json["restart_required"], json!(true));
    let invalid_settings = server
        .patch(&format!("/api/v1/servers/{id}/settings"))
        .authorization_bearer(&t)
        .json(&json!({ "cpu_limit": 32.0 }))
        .await;
    assert_eq!(invalid_settings.status_code(), 422);
    let busy_task = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO tasks (id, type, server_id, state) VALUES (?, 'update', ?, 'queued')")
        .bind(&busy_task)
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let busy_settings = server
        .patch(&format!("/api/v1/servers/{id}/settings"))
        .authorization_bearer(&t)
        .json(&json!({ "cpu_limit": 3.0 }))
        .await;
    assert_eq!(busy_settings.status_code(), 409);
    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(&busy_task)
        .execute(&pool)
        .await
        .unwrap();

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
    assert_eq!(
        server
            .get(&format!("/api/v1/servers/{id}/settings"))
            .authorization_bearer(&viewer)
            .await
            .status_code(),
        200
    );
    assert_eq!(
        server
            .patch(&format!("/api/v1/servers/{id}/settings"))
            .authorization_bearer(&viewer)
            .json(&json!({ "cpu_limit": 1.0 }))
            .await
            .status_code(),
        403
    );

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

// ---------------------------------------------------------------------------
// Task 2: cookie sessions, CSRF, rate limiting, atomic setup, headers
// ---------------------------------------------------------------------------

fn csrf_token(res: &axum_test::TestResponse) -> String {
    auth::csrf_token_from_set_cookie(res.headers()).expect("pm_csrf cookie")
}

fn set_cookie(res: &axum_test::TestResponse, name: &str) -> String {
    res.headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|raw| raw.starts_with(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} Set-Cookie header"))
        .to_string()
}

#[tokio::test]
async fn login_does_not_return_token_in_body() {
    let (server, _) = test_server().await;
    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200, "setup: {}", res.text());
    let body = res.json::<Value>();
    assert!(
        body.get("token").is_none(),
        "response body must not carry a session token: {body}"
    );
    assert!(body["user"]["id"].is_string());
    assert_eq!(body["user"]["username"], json!("owner"));
    assert_eq!(body["user"]["role"], json!("owner"));
    // The token still reaches the client, via the Set-Cookie header.
    assert!(auth::session_token_from_set_cookie(res.headers()).is_some());
}

#[tokio::test]
async fn cookie_is_httponly_and_secure() {
    let (server, _) = test_server().await;
    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200);

    let session = set_cookie(&res, "pm_session");
    assert!(session.contains("HttpOnly"), "session cookie: {session}");
    assert!(
        session.contains("SameSite=Lax"),
        "session cookie: {session}"
    );
    assert!(session.contains("Path=/"), "session cookie: {session}");
    assert!(session.contains("Secure"), "session cookie: {session}");

    let csrf = set_cookie(&res, "pm_csrf");
    assert!(
        !csrf.contains("HttpOnly"),
        "csrf cookie must be JS-readable: {csrf}"
    );
    assert!(csrf.contains("Secure"), "csrf cookie: {csrf}");
}

#[tokio::test]
async fn insecure_cookie_mode_omits_secure_and_hsts() {
    let pool = test_pool().await;
    let root = std::env::temp_dir().join(format!("pm-insecure-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let state = AppState::for_tests(pool, root).with_insecure_cookies();
    let server = TestServer::new(build_router(state)).unwrap();

    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200);
    assert!(!set_cookie(&res, "pm_session").contains("Secure"));
    assert!(!set_cookie(&res, "pm_csrf").contains("Secure"));

    let live = server.get("/api/v1/health/live").await;
    assert!(
        live.headers().get("strict-transport-security").is_none(),
        "HSTS must be suppressed in insecure-cookie mode"
    );
}

async fn setup_and_cookies(server: &TestServer) -> (String, String) {
    let res = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner", "password": "supersecret1"}))
        .await;
    assert_eq!(res.status_code(), 200, "setup: {}", res.text());
    (session_token(&res), csrf_token(&res))
}

fn cookie_header(token: &str, csrf: &str) -> String {
    format!("pm_session={token}; pm_csrf={csrf}")
}

#[tokio::test]
async fn mutation_without_csrf_403() {
    let (server, _) = test_server().await;
    let (token, csrf) = setup_and_cookies(&server).await;
    let res = server
        .post("/api/v1/servers")
        .add_header("cookie", cookie_header(&token, &csrf))
        .json(&json!({"name": "csrf-less", "game_port": 25590,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(res.status_code(), 403, "body: {}", res.text());
    assert_eq!(res.json::<Value>()["error_code"], json!("CSRF_FAILED"));
}

#[tokio::test]
async fn mutation_with_csrf_ok() {
    let (server, _) = test_server().await;
    let (token, csrf) = setup_and_cookies(&server).await;
    let res = server
        .post("/api/v1/servers")
        .add_header("cookie", cookie_header(&token, &csrf))
        .add_header("x-csrf-token", &csrf)
        .json(&json!({"name": "csrf-ok", "game_port": 25591,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(res.status_code(), 201, "body: {}", res.text());
}

#[tokio::test]
async fn bearer_mutation_needs_no_csrf() {
    let (server, _) = test_server().await;
    let token = setup_owner(&server).await;
    let res = server
        .post("/api/v1/servers")
        .authorization_bearer(&token)
        .json(&json!({"name": "bearer-mut", "game_port": 25592,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(res.status_code(), 201, "body: {}", res.text());
}

#[tokio::test]
async fn login_rejects_cross_origin_origin_header() {
    let (server, pool) = test_server().await;
    make_user(&pool, "originuser", "admin", "originpw123").await;
    let res = server
        .post("/api/v1/auth/login")
        .add_header("origin", "https://evil.example")
        .json(&json!({"username": "originuser", "password": "originpw123"}))
        .await;
    assert_eq!(res.status_code(), 403, "body: {}", res.text());
    assert_eq!(res.json::<Value>()["error_code"], json!("CSRF_FAILED"));
}

#[tokio::test]
async fn login_rate_limited() {
    let (server, pool) = test_server().await;
    make_user(&pool, "rluser", "admin", "rlsecret123").await;

    let mut statuses = Vec::new();
    for _ in 0..6 {
        let res = server
            .post("/api/v1/auth/login")
            .json(&json!({"username": "rluser", "password": "wrongpass1"}))
            .await;
        statuses.push(res.status_code().as_u16());
        if res.status_code() == 429 {
            assert_eq!(res.json::<Value>()["error_code"], json!("RATE_LIMITED"));
            assert!(
                res.headers().get("retry-after").is_some(),
                "429 must carry Retry-After"
            );
        }
    }
    assert_eq!(
        statuses,
        vec![401, 401, 401, 401, 401, 429],
        "5 attempts allowed, 6th limited"
    );
}

#[tokio::test]
async fn concurrent_setup_single_owner() {
    let (server, pool) = test_server().await;
    let req_a = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "ownerA", "password": "supersecret1"}));
    let req_b = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "ownerB", "password": "supersecret1"}));
    let (ra, rb) = tokio::join!(req_a, req_b);

    let codes = [ra.status_code().as_u16(), rb.status_code().as_u16()];
    assert_eq!(
        codes.iter().filter(|c| **c == 200).count(),
        1,
        "exactly one setup may win: {codes:?}"
    );
    assert_eq!(
        codes.iter().filter(|c| **c == 409).count(),
        1,
        "the loser must get 409: {codes:?}"
    );

    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(users, 1, "only one owner may be created");
    let status = server.get("/api/v1/auth/setup-status").await;
    assert_eq!(status.json::<Value>()["needs_setup"], json!(false));
}

#[tokio::test]
async fn setup_disabled_after_complete() {
    let (server, pool) = test_server().await;
    let _ = setup_owner(&server).await;

    // Emptied users table must not re-enable setup: the flag is authoritative.
    sqlx::query("DELETE FROM users")
        .execute(&pool)
        .await
        .unwrap();
    let status = server.get("/api/v1/auth/setup-status").await;
    assert_eq!(status.json::<Value>()["needs_setup"], json!(false));

    let again = server
        .post("/api/v1/auth/setup")
        .json(&json!({"username": "owner2", "password": "supersecret1"}))
        .await;
    assert_eq!(again.status_code(), 409);
    assert_eq!(
        again.json::<Value>()["error_code"],
        json!("SETUP_ALREADY_COMPLETE")
    );
}

#[tokio::test]
async fn unknown_user_runs_dummy_hash() {
    use std::sync::atomic::Ordering;
    let (server, _) = test_server().await;
    let before = auth::DUMMY_HASH_CHECKS.load(Ordering::Relaxed);
    let res = server
        .post("/api/v1/auth/login")
        .json(&json!({"username": "ghost-user", "password": "whatever123"}))
        .await;
    assert_eq!(res.status_code(), 401);
    assert_eq!(
        res.json::<Value>()["error_code"],
        json!("INVALID_CREDENTIALS")
    );
    let after = auth::DUMMY_HASH_CHECKS.load(Ordering::Relaxed);
    assert!(
        after > before,
        "unknown-user login must burn a dummy Argon2id verification"
    );
}

#[tokio::test]
async fn security_headers_present() {
    let (server, _) = test_server().await;
    let res = server.get("/api/v1/health/live").await;
    let headers = res.headers();
    let csp = headers
        .get("content-security-policy")
        .expect("CSP")
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'self'"), "csp: {csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "csp: {csp}");
    assert!(
        !csp.contains("unsafe-eval"),
        "csp must not allow eval: {csp}"
    );
    assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    assert!(headers.get("permissions-policy").is_some());
    assert!(headers.get("strict-transport-security").is_some());
}

#[tokio::test]
async fn security_headers_present_on_static_asset() {
    let pool = test_pool().await;
    let root = std::env::temp_dir().join(format!("pm-static-{}", uuid::Uuid::new_v4()));
    let web = root.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<html>STATIC-MARKER</html>").unwrap();
    let state = AppState::for_tests(pool, root.clone());
    let server = TestServer::new(build_router(state)).unwrap();

    let res = server.get("/").await;
    assert_eq!(res.status_code(), 200, "body: {}", res.text());
    assert!(res.text().contains("STATIC-MARKER"));
    let headers = res.headers();
    assert!(
        headers.get("content-security-policy").is_some(),
        "CSP missing on static asset response"
    );
    assert_eq!(
        headers.get("x-content-type-options").unwrap(),
        "nosniff",
        "nosniff missing on static asset response"
    );
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    assert!(headers.get("strict-transport-security").is_some());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn logout_clears_both_cookies() {
    let (server, _) = test_server().await;
    let (token, csrf) = setup_and_cookies(&server).await;
    let res = server
        .post("/api/v1/auth/logout")
        .add_header("cookie", cookie_header(&token, &csrf))
        .add_header("x-csrf-token", &csrf)
        .await;
    assert_eq!(res.status_code(), 204, "body: {}", res.text());
    let session = set_cookie(&res, "pm_session");
    let csrf_cookie = set_cookie(&res, "pm_csrf");
    assert!(
        session.contains("pm_session=;"),
        "value not cleared: {session}"
    );
    assert!(session.contains("Max-Age=0"), "not expired: {session}");
    assert!(
        csrf_cookie.contains("pm_csrf=;"),
        "value not cleared: {csrf_cookie}"
    );
    assert!(
        csrf_cookie.contains("Max-Age=0"),
        "not expired: {csrf_cookie}"
    );
}

// ---------------------------------------------------------------------------
// Task 3: delete busy guard + op-lock release, unknown-field rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_refuses_busy_and_releases_op_lock() {
    let pool = test_pool().await;
    let root = std::env::temp_dir().join(format!("pm-busy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let state = AppState::for_tests(pool.clone(), root);
    let locks = state.op_locks.clone();
    let server = TestServer::new(build_router(state)).unwrap();
    let t = setup_owner(&server).await;

    let created = server
        .post("/api/v1/servers")
        .authorization_bearer(&t)
        .json(&json!({"name": "busy-del", "game_port": 25610,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(created.status_code(), 201, "body: {}", created.text());
    let id = created.json::<Value>()["server"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // A conflicting queued task must make DELETE fail safe with 409.
    sqlx::query("INSERT INTO tasks (id, type, server_id, state) VALUES (?, 'stop', ?, 'queued')")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let busy = server
        .delete(&format!("/api/v1/servers/{id}"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(busy.status_code(), 409, "body: {}", busy.text());
    assert_eq!(busy.json::<Value>()["error_code"], json!("SERVER_BUSY"));

    // Once the task is terminal the delete proceeds and the op-lock release
    // is observable (guards against a leaked lock on a rejected/accepted op).
    sqlx::query("UPDATE tasks SET state = 'failed' WHERE server_id = ?")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let del = server
        .delete(&format!("/api/v1/servers/{id}"))
        .authorization_bearer(&t)
        .await;
    assert_eq!(del.status_code(), 204, "body: {}", del.text());

    // The handler released the per-server lock (deferred drop → `acquire`
    // waits for the slot rather than racing it).
    let freed = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::op_lock::acquire(&locks, &id),
    )
    .await
    .expect("delete handler must release the op-lock");
    drop(freed);
}

#[tokio::test]
async fn create_rejects_unknown_field() {
    let (server, _) = test_server().await;
    let t = setup_owner(&server).await;
    let res = server
        .post("/api/v1/servers")
        .authorization_bearer(&t)
        .json(&json!({"name": "smuggle", "game_port": 25611,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master",
                      "privileged": true}))
        .await;
    assert!(
        matches!(res.status_code().as_u16(), 400 | 422),
        "unknown field must be rejected, got {}: {}",
        res.status_code(),
        res.text()
    );
}

#[tokio::test]
async fn delete_rejects_unknown_query_field() {
    let (server, _) = test_server().await;
    let t = setup_owner(&server).await;
    let created = server
        .post("/api/v1/servers")
        .authorization_bearer(&t)
        .json(&json!({"name": "qfield", "game_port": 25612,
                      "image_reference": "ghcr.io/pumpkin-mc/pumpkin:master"}))
        .await;
    assert_eq!(created.status_code(), 201);
    let id = created.json::<Value>()["server"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = server
        .delete(&format!("/api/v1/servers/{id}?evil=1"))
        .authorization_bearer(&t)
        .await;
    assert!(
        matches!(res.status_code().as_u16(), 400 | 422),
        "unknown query field must be rejected, got {}: {}",
        res.status_code(),
        res.text()
    );
}
