//! Metrics history + alerts endpoints (Phase 6, Task 2).
//!
//! - `GET /api/v1/servers/:id/metrics?range=1h|24h|7d` — newest-first samples
//!   from `metrics_samples` (Task 1 table, frozen shape), capped at 1000 rows.
//!   Unavailable metrics stay `null` end-to-end; never 0-filled.
//! - `GET /api/v1/alerts` — stateless derived alerts (never stored, never
//!   stale). A single failed ping or a single failed start NEVER alerts
//!   (flap guard); only sustained, DB-evidenced failure does.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::{auth::AuthUser, metrics_collector::server_samples, AppState};

fn error(status: StatusCode, code: &'static str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error_code": code,
            "message": message,
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_range")]
    pub range: String,
}

fn default_range() -> String {
    "24h".to_string()
}

/// `?range=` window in seconds. Only the frozen Task 4 contract values parse;
/// anything else is a 400, never a silent default.
fn range_seconds(range: &str) -> Option<i64> {
    match range.trim() {
        "1h" => Some(3600),
        "24h" => Some(24 * 3600),
        "7d" => Some(7 * 24 * 3600),
        _ => None,
    }
}

/// UTC RFC 3339 (millis) `secs` ago — lexically comparable with the
/// `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')` timestamps the DB stores.
fn utc_cutoff(secs_ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(secs_ago))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// GET /api/v1/servers/:id/metrics?range=1h|24h|7d — newest-first samples,
/// capped at 1000 rows. `total` is the number of samples returned.
/// Unavailable metrics stay `null`; unknown servers 404.
pub async fn server_metrics(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let window = match range_seconds(&query.range) {
        Some(s) => s,
        None => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_RANGE",
                "range must be one of 1h, 24h, 7d",
            );
        }
    };
    let exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM servers WHERE id = ? AND deleted_at IS NULL")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
    if exists == 0 {
        return error(StatusCode::NOT_FOUND, "SERVER_NOT_FOUND", "no such server");
    }
    // History reads go through Task 1's canonical `server_samples`
    // (newest-first, 1000-capped, NULL-preserving) — no second query here.
    // A history-query failure is 500, never an empty 200: the operator must
    // be able to tell "no data yet" from "database broken".
    let since = utc_cutoff(window);
    let rows = match server_samples(&state.pool, &id, &since).await {
        Ok(rows) => rows,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                "failed to load metrics history",
            );
        }
    };
    let samples: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "sampled_at": r.sampled_at,
                "cpu_percent": r.cpu_percent,
                "memory_bytes": r.memory_bytes,
                "players_online": r.players_online,
                "latency_ms": r.latency_ms,
                "state": r.state,
            })
        })
        .collect();
    let total = samples.len();
    Json(serde_json::json!({ "samples": samples, "total": total })).into_response()
}

#[derive(sqlx::FromRow)]
struct AlertServerRow {
    id: String,
    name: String,
    image_reference: String,
    created_at: String,
}

/// GET /api/v1/alerts — stateless derived alerts, recomputed on every read
/// (nothing stored, nothing stale). Crash/backup/image/backup-age rules read
/// the DB; disk pressure reads live host stats. Readiness pings and single
/// failures never feed any rule (flap guard).
pub async fn list_alerts(State(state): State<AppState>, _user: AuthUser) -> Response {
    let mut alerts = Vec::new();
    if let Some(alert) = disk_pressure_alert().await {
        alerts.push(alert);
    }
    let servers = sqlx::query_as::<_, AlertServerRow>(
        "SELECT id, name, image_reference, created_at
         FROM servers WHERE deleted_at IS NULL ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let crash_cutoff = utc_cutoff(3600);
    let old_cutoff = utc_cutoff(7 * 24 * 3600);
    for server in servers {
        if !server.image_reference.contains("@sha256:") {
            alerts.push(serde_json::json!({
                "severity": "info",
                "kind": "unverified-image",
                "server_id": server.id,
                "message": format!(
                    "server '{}' uses image '{}' without a pinned digest; mutable tags can drift between pulls",
                    server.name, server.image_reference,
                ),
            }));
        }
        let crashes: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tasks
             WHERE server_id = ? AND type = 'start'
               AND state = 'failed' AND created_at >= ?",
        )
        .bind(&server.id)
        .bind(&crash_cutoff)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
        if crashes >= 3 {
            alerts.push(serde_json::json!({
                "severity": "warning",
                "kind": "repeated-crashes",
                "server_id": server.id,
                "message": format!(
                    "server '{}' failed to start {} times in the last hour",
                    server.name, crashes,
                ),
            }));
        }
        let latest_backup: Option<String> = sqlx::query_scalar(
            "SELECT state FROM tasks
             WHERE server_id = ? AND type = 'backup'
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&server.id)
        .fetch_optional(&state.pool)
        .await
        .unwrap_or(None);
        if latest_backup.as_deref() == Some("failed") {
            alerts.push(serde_json::json!({
                "severity": "warning",
                "kind": "failed-backup",
                "server_id": server.id,
                "message": format!("server '{}' latest backup task failed", server.name),
            }));
        }
        if server.created_at < old_cutoff {
            let completed: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM backups WHERE server_id = ? AND state = 'completed'",
            )
            .bind(&server.id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);
            if completed == 0 {
                alerts.push(serde_json::json!({
                    "severity": "info",
                    "kind": "missing-backup",
                    "server_id": server.id,
                    "message": format!(
                        "server '{}' is over 7 days old and has no completed backups",
                        server.name,
                    ),
                }));
            }
        }
    }
    Json(serde_json::json!({ "alerts": alerts })).into_response()
}

/// Live host disk check: warning when the root filesystem has <10% free or
/// <5 GiB free. `None` = healthy or unreadable (unknown reads as absent,
/// never as pressure).
async fn disk_pressure_alert() -> Option<serde_json::Value> {
    tokio::task::spawn_blocking(|| {
        use sysinfo::Disks;
        let disks = Disks::new_with_refreshed_list();
        let root = disks
            .iter()
            .find(|d| d.mount_point().to_string_lossy() == "/")
            .or_else(|| disks.first())?;
        let total = root.total_space();
        let free = root.available_space();
        if total == 0 {
            return None;
        }
        if free * 10 >= total && free >= 5 * 1024 * 1024 * 1024 {
            return None;
        }
        const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
        Some(serde_json::json!({
            "severity": "warning",
            "kind": "disk-pressure",
            "server_id": null,
            "message": format!(
                "host disk space is low ({:.1} GiB free of {:.1} GiB)",
                free as f64 / GIB,
                total as f64 / GIB,
            ),
        }))
    })
    .await
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------
// Tests (written first — they fail against the stubs above)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum_test::TestServer;
    use serde_json::{json, Value};
    use sqlx::SqlitePool;

    /// Task 1 `metrics_samples` DDL, verbatim from the Phase 6 plan (Task 1
    /// owns the real migration; tests stand it up locally so history queries
    /// run against the frozen shape).
    const METRICS_SQL: &str = "CREATE TABLE metrics_samples (id TEXT PRIMARY KEY, server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE, sampled_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')), cpu_percent REAL, memory_usage_bytes INTEGER, memory_limit_bytes INTEGER, restart_count INTEGER NOT NULL DEFAULT -1, players_online INTEGER, latency_ms INTEGER, state TEXT NOT NULL DEFAULT 'unknown');\nCREATE INDEX idx_metrics_server_time ON metrics_samples(server_id, sampled_at);";

    const PINNED_IMAGE: &str = "ghcr.io/pumpkin-mc/pumpkin@sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();
        for file in [
            include_str!("../../../migrations/0001_init.sql"),
            include_str!("../../../migrations/0002_sessions_audit.sql"),
            include_str!("../../../migrations/0005_backups.sql"),
            include_str!("../../../migrations/0009_app_settings.sql"),
            METRICS_SQL,
        ] {
            // Strip full-line `--` comments first: some carry `;` inside
            // (e.g. "immutable; new tables only"), which would corrupt a
            // naive `;` split.
            let mut sql = String::new();
            for line in file.lines() {
                if line.trim_start().starts_with("--") {
                    continue;
                }
                sql.push_str(line);
                sql.push('\n');
            }
            for stmt in sql.split(';') {
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                let upper = stmt.to_uppercase();
                if !upper.contains("CREATE") && !upper.contains("INDEX") {
                    continue;
                }
                sqlx::query(stmt).execute(&pool).await.unwrap();
            }
        }
        pool
    }

    async fn test_router_server() -> (TestServer, SqlitePool) {
        let pool = test_pool().await;
        let root = std::env::temp_dir().join(format!("pm6t2-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState::for_tests(pool.clone(), root);
        // Routes are wired in `build_router` with the production auth +
        // permission gates; use it directly (no second test router merge).
        let app = crate::build_router(state);
        (TestServer::new(app).unwrap(), pool)
    }

    fn ts_ago(secs: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    async fn owner_token(server: &TestServer) -> String {
        let res = server
            .post("/api/v1/auth/setup")
            .json(&json!({"username": "owner", "password": "supersecret1"}))
            .await;
        assert_eq!(res.status_code(), 200, "setup failed: {}", res.text());
        crate::auth::session_token_from_set_cookie(res.headers())
            .expect("pm_session cookie after setup")
    }

    async fn seed_server(
        pool: &SqlitePool,
        name: &str,
        port: i64,
        image: &str,
        created_at: &str,
    ) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO servers (id, name, slug, image_reference, game_port, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(format!("{name}-slug"))
        .bind(image)
        .bind(port)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_sample(
        pool: &SqlitePool,
        server_id: &str,
        sampled_at: &str,
        cpu: Option<f64>,
        mem: Option<i64>,
        players: Option<i64>,
        latency: Option<i64>,
        state: &str,
    ) {
        sqlx::query(
            "INSERT INTO metrics_samples
                (id, server_id, sampled_at, cpu_percent, memory_usage_bytes,
                 players_online, latency_ms, state)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(server_id)
        .bind(sampled_at)
        .bind(cpu)
        .bind(mem)
        .bind(players)
        .bind(latency)
        .bind(state)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_task(
        pool: &SqlitePool,
        kind: &str,
        server_id: &str,
        state: &str,
        created_at: Option<&str>,
    ) {
        let created = created_at.map(str::to_string).unwrap_or_else(|| ts_ago(0));
        sqlx::query(
            "INSERT INTO tasks (id, type, server_id, state, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(kind)
        .bind(server_id)
        .bind(state)
        .bind(created)
        .execute(pool)
        .await
        .unwrap();
    }

    fn has_alert(body: &Value, kind: &str, server_id: Option<&str>) -> bool {
        body["alerts"].as_array().is_some_and(|alerts| {
            alerts
                .iter()
                .any(|a| a["kind"] == kind && server_id.is_none_or(|id| a["server_id"] == id))
        })
    }

    #[tokio::test]
    async fn history_range_parsing() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        let id = seed_server(&pool, "hist", 25565, PINNED_IMAGE, &ts_ago(0)).await;
        seed_sample(
            &pool,
            &id,
            &ts_ago(600),
            Some(12.5),
            Some(1024),
            Some(3),
            Some(42),
            "running",
        )
        .await;
        seed_sample(
            &pool,
            &id,
            &ts_ago(1800),
            Some(8.0),
            Some(900),
            None,
            None,
            "running",
        )
        .await;
        seed_sample(
            &pool,
            &id,
            &ts_ago(2 * 3600),
            None,
            None,
            None,
            None,
            "missing",
        )
        .await;
        seed_sample(
            &pool,
            &id,
            &ts_ago(2 * 86400),
            Some(5.0),
            Some(500),
            Some(1),
            Some(10),
            "running",
        )
        .await;

        // 1h → the two newest, newest-first.
        let res = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=1h"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["total"], json!(2));
        let samples = body["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 2);
        assert!(
            samples[0]["sampled_at"].as_str().unwrap() > samples[1]["sampled_at"].as_str().unwrap(),
            "newest-first"
        );
        // NULL ping fields stay null (never 0-filled), cpu present.
        assert_eq!(samples[1]["players_online"], json!(null));
        assert_eq!(samples[1]["latency_ms"], json!(null));
        assert!(samples[1]["cpu_percent"].is_number());

        // 24h → 3 rows; all-null row preserved as nulls, not zeros.
        let res = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=24h"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["total"], json!(3));
        let oldest = body["samples"].as_array().unwrap().last().unwrap();
        assert_eq!(oldest["state"], json!("missing"));
        assert_eq!(oldest["cpu_percent"], json!(null));
        assert_eq!(oldest["memory_bytes"], json!(null));

        // 7d → all 4; omitted range defaults to 24h.
        let res = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=7d"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.json::<Value>()["total"], json!(4));
        let res = server
            .get(&format!("/api/v1/servers/{id}/metrics"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        assert_eq!(res.json::<Value>()["total"], json!(3));

        // Garbage range → 400; unknown server → 404; anonymous → 401.
        let bad = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=fortnight"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(bad.status_code(), 400);
        let missing = server
            .get("/api/v1/servers/does-not-exist/metrics?range=1h")
            .authorization_bearer(&t)
            .await;
        assert_eq!(missing.status_code(), 404);
        let anon = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=1h"))
            .await;
        assert_eq!(anon.status_code(), 401);
    }

    #[tokio::test]
    async fn history_caps_at_1000_rows() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        let id = seed_server(&pool, "bighist", 25566, PINNED_IMAGE, &ts_ago(0)).await;
        for i in 0..1100 {
            seed_sample(
                &pool,
                &id,
                &ts_ago(i64::from(i) * 60),
                Some(f64::from(i)),
                Some(1000),
                Some(1),
                Some(5),
                "running",
            )
            .await;
        }
        let res = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=7d"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["total"], json!(1000));
        let samples = body["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 1000);
        assert!(
            samples.first().unwrap()["sampled_at"].as_str().unwrap()
                > samples.last().unwrap()["sampled_at"].as_str().unwrap(),
            "capped page stays newest-first"
        );
    }

    #[tokio::test]
    async fn history_matches_canonical_server_samples() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        let id = seed_server(&pool, "single-source", 25567, PINNED_IMAGE, &ts_ago(0)).await;
        seed_sample(
            &pool,
            &id,
            &ts_ago(300),
            Some(12.5),
            Some(1024),
            Some(3),
            Some(42),
            "running",
        )
        .await;
        seed_sample(&pool, &id, &ts_ago(900), None, None, None, None, "missing").await;
        seed_sample(
            &pool,
            &id,
            &ts_ago(1500),
            Some(8.0),
            None,
            None,
            Some(7),
            "running",
        )
        .await;

        let res = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=1h"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        let samples = body["samples"].as_array().unwrap().clone();

        // Canonical rows for the same window, recomputed independently.
        let since = super::utc_cutoff(3600);
        let rows = crate::metrics_collector::server_samples(&pool, &id, &since)
            .await
            .expect("history reads succeed");
        assert_eq!(samples.len(), rows.len(), "handler mirrors canonical rows");
        for (json_row, row) in samples.iter().zip(rows.iter()) {
            assert_eq!(json_row["sampled_at"], json!(row.sampled_at));
            assert_eq!(json_row["cpu_percent"], json!(row.cpu_percent));
            assert_eq!(json_row["memory_bytes"], json!(row.memory_bytes));
            assert_eq!(json_row["players_online"], json!(row.players_online));
            assert_eq!(json_row["latency_ms"], json!(row.latency_ms));
            assert_eq!(json_row["state"], json!(row.state));
        }
        // NULLs preserved end-to-end, never 0-filled.
        assert!(samples.iter().any(|s| s["cpu_percent"] == json!(null)));
    }

    #[tokio::test]
    async fn history_db_failure_is_500() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        let id = seed_server(&pool, "dbfail", 25569, PINNED_IMAGE, &ts_ago(0)).await;
        sqlx::query("DROP TABLE metrics_samples")
            .execute(&pool)
            .await
            .unwrap();
        let res = server
            .get(&format!("/api/v1/servers/{id}/metrics?range=1h"))
            .authorization_bearer(&t)
            .await;
        assert_eq!(res.status_code(), 500, "body: {}", res.text());
        let body = res.json::<Value>();
        assert_eq!(body["error_code"], json!("INTERNAL"));
    }

    #[tokio::test]
    async fn repeated_crashes_fire_honestly() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        let now = ts_ago(0);

        // A single failed start NEVER alerts (flap guard); an exited sample
        // row must not manufacture a crash alert either.
        let flapping = seed_server(&pool, "flap", 25561, PINNED_IMAGE, &now).await;
        seed_task(&pool, "start", &flapping, "failed", None).await;
        seed_sample(&pool, &flapping, &now, None, None, None, None, "exited").await;

        // Sustained failure: ≥3 failed starts within the hour → alert.
        let crashy = seed_server(&pool, "crashy", 25562, PINNED_IMAGE, &now).await;
        for _ in 0..3 {
            seed_task(&pool, "start", &crashy, "failed", None).await;
        }

        // Two failures are not sustained; stale (>1h) failures do not count.
        let wobbly = seed_server(&pool, "wobbly", 25563, PINNED_IMAGE, &now).await;
        for _ in 0..2 {
            seed_task(&pool, "start", &wobbly, "failed", None).await;
        }
        let stale = seed_server(&pool, "stale", 25564, PINNED_IMAGE, &now).await;
        for _ in 0..3 {
            seed_task(&pool, "start", &stale, "failed", Some(&ts_ago(7200))).await;
        }

        let res = server.get("/api/v1/alerts").authorization_bearer(&t).await;
        assert_eq!(res.status_code(), 200, "body: {}", res.text());
        let body = res.json::<Value>();
        assert!(has_alert(&body, "repeated-crashes", Some(&crashy)));
        assert!(!has_alert(&body, "repeated-crashes", Some(&flapping)));
        assert!(!has_alert(&body, "repeated-crashes", Some(&wobbly)));
        assert!(!has_alert(&body, "repeated-crashes", Some(&stale)));
        let alert = body["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "repeated-crashes")
            .unwrap();
        assert_eq!(alert["severity"], json!("warning"));
        assert_eq!(alert["server_id"], json!(crashy));
        assert!(alert["message"].as_str().unwrap().contains("crashy"));
    }

    #[tokio::test]
    async fn failed_backup_tracks_latest_task() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        let now = ts_ago(0);

        let failed = seed_server(&pool, "bkfail", 25571, PINNED_IMAGE, &now).await;
        seed_task(&pool, "backup", &failed, "failed", None).await;

        // An older failure superseded by success → no alert.
        let recovered = seed_server(&pool, "bkok", 25572, PINNED_IMAGE, &now).await;
        seed_task(&pool, "backup", &recovered, "failed", Some(&ts_ago(7200))).await;
        seed_task(&pool, "backup", &recovered, "succeeded", None).await;

        // No backup tasks at all → no alert.
        let never = seed_server(&pool, "bknever", 25573, PINNED_IMAGE, &now).await;

        let body = server
            .get("/api/v1/alerts")
            .authorization_bearer(&t)
            .await
            .json::<Value>();
        assert!(has_alert(&body, "failed-backup", Some(&failed)));
        assert!(!has_alert(&body, "failed-backup", Some(&recovered)));
        assert!(!has_alert(&body, "failed-backup", Some(&never)));
        let alert = body["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "failed-backup")
            .unwrap();
        assert_eq!(alert["severity"], json!("warning"));
    }

    #[tokio::test]
    async fn unverified_image_tracks_digest_pin() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        let now = ts_ago(0);

        let loose = seed_server(
            &pool,
            "loose",
            25574,
            "ghcr.io/pumpkin-mc/pumpkin:master",
            &now,
        )
        .await;
        let pinned = seed_server(&pool, "pinned", 25575, PINNED_IMAGE, &now).await;

        let body = server
            .get("/api/v1/alerts")
            .authorization_bearer(&t)
            .await
            .json::<Value>();
        assert!(has_alert(&body, "unverified-image", Some(&loose)));
        assert!(!has_alert(&body, "unverified-image", Some(&pinned)));
        let alert = body["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "unverified-image")
            .unwrap();
        assert_eq!(alert["severity"], json!("info"));
    }

    #[tokio::test]
    async fn missing_backup_needs_age_and_zero_completed() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;

        // Old server, zero completed backups → alert.
        let old = seed_server(&pool, "oldie", 25576, PINNED_IMAGE, &ts_ago(8 * 86400)).await;
        // Young server without backups → no alert.
        let young = seed_server(&pool, "young", 25577, PINNED_IMAGE, &ts_ago(0)).await;
        // Old server with a completed backup → no alert.
        let backed = seed_server(&pool, "backed", 25578, PINNED_IMAGE, &ts_ago(8 * 86400)).await;
        sqlx::query("INSERT INTO backups (id, server_id, state) VALUES (?, ?, 'completed')")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&backed)
            .execute(&pool)
            .await
            .unwrap();
        // A failed backup row is not a completed backup → still alerts.
        let failedonly =
            seed_server(&pool, "failedonly", 25579, PINNED_IMAGE, &ts_ago(8 * 86400)).await;
        sqlx::query("INSERT INTO backups (id, server_id, state) VALUES (?, ?, 'failed')")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&failedonly)
            .execute(&pool)
            .await
            .unwrap();

        let body = server
            .get("/api/v1/alerts")
            .authorization_bearer(&t)
            .await
            .json::<Value>();
        assert!(has_alert(&body, "missing-backup", Some(&old)));
        assert!(has_alert(&body, "missing-backup", Some(&failedonly)));
        assert!(!has_alert(&body, "missing-backup", Some(&young)));
        assert!(!has_alert(&body, "missing-backup", Some(&backed)));
        let alert = body["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["kind"] == "missing-backup")
            .unwrap();
        assert_eq!(alert["severity"], json!("info"));
    }

    #[tokio::test]
    async fn disk_pressure_matches_live_host() {
        let (server, pool) = test_router_server().await;
        let t = owner_token(&server).await;
        seed_server(&pool, "plain", 25580, PINNED_IMAGE, &ts_ago(0)).await;

        // The rule is live-only: expect exactly what the host reports now.
        let expect_pressure = tokio::task::spawn_blocking(|| {
            use sysinfo::Disks;
            let disks = Disks::new_with_refreshed_list();
            let root = disks
                .iter()
                .find(|d| d.mount_point().to_string_lossy() == "/")
                .or_else(|| disks.first());
            match root {
                Some(r) if r.total_space() > 0 => {
                    let free = r.available_space();
                    free * 10 < r.total_space() || free < 5 * 1024 * 1024 * 1024
                }
                _ => false,
            }
        })
        .await
        .unwrap();

        let body = server
            .get("/api/v1/alerts")
            .authorization_bearer(&t)
            .await
            .json::<Value>();
        assert_eq!(has_alert(&body, "disk-pressure", None), expect_pressure);

        // Alerts sit behind ServerRead like the rest of the API.
        let anon = server.get("/api/v1/alerts").await;
        assert_eq!(anon.status_code(), 401);
    }
}
