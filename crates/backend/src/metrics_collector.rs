//! Phase 6 metrics sampling + bounded retention.
//!
//! [`sample_all`] writes exactly one [`metrics_samples`] row per live server
//! per call: live data for running containers, `NULL` metric fields with a
//! truthful `state` for dead/stopped/unreachable ones (never zeros).
//! [`prune_samples`] then enforces the 7-day / 50k-row bounds.

use sqlx::SqlitePool;

use crate::readiness::{ping_java, probe_host};
use crate::runtime::{RuntimeAdapter, RuntimeError};

/// Samples older than this are pruned (whichever bound bites first).
pub const SAMPLE_RETENTION_DAYS: i64 = 7;
/// Per-server cap on `metrics_samples` (one row/minute for seven days).
pub const SAMPLE_PER_SERVER_CAP: i64 = SAMPLE_RETENTION_DAYS * 24 * 60;

/// Cap on rows returned by [`server_samples`].
const HISTORY_LIMIT: i64 = 1000;

/// Sampled point: (state, cpu, mem_used, mem_limit, restart_count, players, latency).
type SamplePoint = (
    String,
    Option<f64>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
);

/// One history point for a server (subset of `metrics_samples` columns
/// consumed by Tasks 2-4 + the history controller).
#[derive(Debug, Clone)]
pub struct SampleRow {
    pub sampled_at: String,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<i64>,
    pub players_online: Option<i64>,
    pub latency_ms: Option<i64>,
    pub state: String,
}

/// Sample every live server once, then enforce retention bounds.
///
/// Generic over [`RuntimeAdapter`] (same pattern as `dispatch_task`) so the
/// unreachable-daemon path is unit-testable without a socket.
pub async fn sample_all<A: RuntimeAdapter>(pool: &SqlitePool, adapter: Option<&A>) {
    let servers: Vec<(String, String, i64, String, String)> = sqlx::query_as(
        "SELECT id, container_id, game_port, observed_state, bind_address \
         FROM servers WHERE deleted_at IS NULL",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, container_id, game_port, observed_state, bind_address) in &servers {
        sample_one(
            pool,
            adapter,
            id,
            container_id,
            *game_port,
            observed_state,
            bind_address,
        )
        .await;
    }
    prune_samples(pool).await;
}

/// One row per server per call: live data when the container is running,
/// `NULL` metric fields with a truthful `state` otherwise (never zeros).
async fn sample_one<A: RuntimeAdapter>(
    pool: &SqlitePool,
    adapter: Option<&A>,
    server_id: &str,
    container_id: &str,
    game_port: i64,
    observed_state: &str,
    bind_address: &str,
) {
    // (state, cpu, mem_used, mem_limit, restart_count, players, latency)
    let row: SamplePoint = if container_id.is_empty() {
        // Never materialised: keep the last observed state.
        let state = if observed_state.is_empty() {
            "unknown".to_string()
        } else {
            observed_state.to_string()
        };
        (state, None, None, None, -1, None, None)
    } else {
        let Some(adapter) = adapter else {
            // No daemon to ask: record nothing rather than fabricating
            // "missing". The gap is truthful and the UI shows it as such.
            return;
        };
        match adapter.inspect(container_id).await {
            Ok(snap) => {
                let state = normalize_state(&snap.status);
                if state == "running" {
                    let (cpu, mem_used, mem_limit) = match adapter.stats(container_id).await {
                        Ok(s) => (
                            Some(s.cpu_percent),
                            Some(s.memory_bytes as i64),
                            Some(s.memory_limit as i64),
                        ),
                        Err(_) => (None, None, None),
                    };
                    let (players, latency) = probe_players(bind_address, game_port).await;
                    (
                        state,
                        cpu,
                        mem_used,
                        mem_limit,
                        snap.restart_count,
                        players,
                        latency,
                    )
                } else {
                    (state, None, None, None, snap.restart_count, None, None)
                }
            }
            Err(RuntimeError::NotFound(_)) => {
                ("missing".to_string(), None, None, None, -1, None, None)
            }
            // Daemon unreachable mid-probe: skip this sample rather than
            // pretending the container was deleted.
            Err(_) => return,
        }
    };
    let (state, cpu, mem_used, mem_limit, restart_count, players, latency) = row;
    let _ = sqlx::query(
        "INSERT INTO metrics_samples \
         (id, server_id, cpu_percent, memory_usage_bytes, memory_limit_bytes, \
          restart_count, players_online, latency_ms, state) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(server_id)
    .bind(cpu)
    .bind(mem_used)
    .bind(mem_limit)
    .bind(restart_count)
    .bind(players)
    .bind(latency)
    .bind(&state)
    .execute(pool)
    .await;
}

/// Best-effort game probe: `None`s on any failure (server still booting,
/// ping refused, port unconfigured). Never fails the sample.
async fn probe_players(bind_address: &str, game_port: i64) -> (Option<i64>, Option<i64>) {
    let port = match u16::try_from(game_port) {
        Ok(p) => p,
        Err(_) => return (None, None),
    };
    match ping_java(
        probe_host(bind_address),
        port,
        std::time::Duration::from_secs(2),
    )
    .await
    {
        Ok(p) => (Some(i64::from(p.players_online)), Some(p.latency_ms as i64)),
        Err(_) => (None, None),
    }
}

/// Delete samples older than [`SAMPLE_RETENTION_DAYS`], then enforce the
/// per-server [`SAMPLE_PER_SERVER_CAP`] (each server keeps its own window).
pub async fn prune_samples(pool: &SqlitePool) {
    let _ = sqlx::query(
        "DELETE FROM metrics_samples \
         WHERE sampled_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)",
    )
    .bind(format!("-{SAMPLE_RETENTION_DAYS} days"))
    .execute(pool)
    .await;
    // One chatty server must not evict another server's history.
    let servers: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT server_id FROM metrics_samples")
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    for (server_id,) in servers {
        let _ = sqlx::query(
            "DELETE FROM metrics_samples
             WHERE server_id = ? AND id NOT IN (
                 SELECT id FROM metrics_samples WHERE server_id = ?
                 ORDER BY sampled_at DESC, rowid DESC LIMIT ?
             )",
        )
        .bind(&server_id)
        .bind(&server_id)
        .bind(SAMPLE_PER_SERVER_CAP)
        .execute(pool)
        .await;
    }
}

/// Newest-first history for one server since `since` (inclusive),
/// capped at [`HISTORY_LIMIT`] rows. DB errors propagate: the history
/// handler maps them to 500 so "database broken" never reads as empty.
pub async fn server_samples(
    pool: &SqlitePool,
    server_id: &str,
    since: &str,
) -> Result<Vec<SampleRow>, sqlx::Error> {
    Ok(sqlx::query_as::<
        _,
        (
            String,
            Option<f64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            String,
        ),
    >(&format!(
        "SELECT sampled_at, cpu_percent, memory_usage_bytes, players_online, latency_ms, state \
             FROM metrics_samples \
             WHERE server_id = ? AND sampled_at >= ? \
             ORDER BY sampled_at DESC, rowid DESC LIMIT {HISTORY_LIMIT}",
    ))
    .bind(server_id)
    .bind(since)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(
        |(sampled_at, cpu_percent, memory_bytes, players_online, latency_ms, state)| SampleRow {
            sampled_at,
            cpu_percent,
            memory_bytes,
            players_online,
            latency_ms,
            state,
        },
    )
    .collect())
}

/// Map a raw Docker status to the manager's observed-state vocabulary
/// (same mapping as `sync_observed` in `main.rs`).
fn normalize_state(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "running" => "running",
        "created" => "created",
        "paused" => "paused",
        "restarting" => "starting",
        "removing" => "stopping",
        "exited" | "dead" => "exited",
        _ => "unknown",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DockerAdapter;

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory pool");
        for file in [
            include_str!("../../../migrations/0001_init.sql"),
            include_str!("../../../migrations/0010_port_reservations.sql"),
            include_str!("../../../migrations/0006_metrics.sql"),
        ] {
            for stmt in file.split(';') {
                let stmt = stmt.trim();
                if stmt.is_empty() {
                    continue;
                }
                sqlx::query(stmt).execute(&pool).await.expect("migrate");
            }
        }
        pool
    }

    async fn seed_server(
        pool: &SqlitePool,
        id: &str,
        container_id: &str,
        game_port: i64,
        observed_state: &str,
    ) {
        sqlx::query(
            "INSERT INTO servers (id, name, slug, container_id, game_port, observed_state) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(format!("srv-{id}"))
        .bind(format!("slug-{id}"))
        .bind(container_id)
        .bind(game_port)
        .bind(observed_state)
        .execute(pool)
        .await
        .expect("seed server");
    }

    #[tokio::test]
    async fn dead_server_samples_nulls_not_zeros() {
        let pool = test_pool().await;
        seed_server(&pool, "dead-1", "ghost-container-id", 25565, "running").await;

        // Daemon gone (no adapter): no fabricated observation. History gets a
        // truthful gap instead of a fake "missing" sample.
        sample_all::<DockerAdapter>(&pool, None).await;
        let rows = server_samples(&pool, "dead-1", "1970-01-01T00:00:00.000Z")
            .await
            .expect("history reads succeed");
        assert_eq!(rows.len(), 0, "no daemon means no observation");
        // A container the daemon reports as NotFound is genuinely missing.
        struct Missing;
        #[allow(clippy::manual_async_fn)]
        impl RuntimeAdapter for Missing {
            fn inspect(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<
                Output = Result<crate::runtime::RuntimeSnapshot, crate::runtime::RuntimeError>,
            > + Send {
                async { Err(crate::runtime::RuntimeError::NotFound("gone".to_string())) }
            }
            fn create(
                &self,
                _spec: &crate::runtime::ValidatedServerSpec,
            ) -> impl std::future::Future<
                Output = Result<crate::runtime::RuntimeHandle, crate::runtime::RuntimeError>,
            > + Send {
                async { panic!("must not run") }
            }
            fn start(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn stop(
                &self,
                _container_id: &str,
                _timeout_secs: u32,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn restart(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn remove(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn logs(
                &self,
                _container_id: &str,
                _tail: usize,
            ) -> impl std::future::Future<Output = Result<Vec<String>, crate::runtime::RuntimeError>>
                   + Send {
                async { panic!("must not run") }
            }
            fn stats(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<
                Output = Result<crate::runtime::RuntimeStats, crate::runtime::RuntimeError>,
            > + Send {
                async { panic!("must not run") }
            }
        }
        sample_all(&pool, Some(&Missing)).await;
        let rows = server_samples(&pool, "dead-1", "1970-01-01T00:00:00.000Z")
            .await
            .expect("history reads succeed");
        assert_eq!(rows.len(), 1, "a missing container records one row");
        assert_eq!(rows[0].state, "missing");
        assert!(
            rows[0].cpu_percent.is_none()
                && rows[0].memory_bytes.is_none()
                && rows[0].players_online.is_none()
                && rows[0].latency_ms.is_none(),
            "dead servers get NULLs, never zeros: {:?}",
            rows[0]
        );

        // Daemon unreachable (inspect fails with a non-NotFound error) is
        // covered by the skip above; this stub proves NotFound -> missing.
        struct Unreachable;
        #[allow(clippy::manual_async_fn)] // trait declares `-> impl Future`
        impl RuntimeAdapter for Unreachable {
            fn inspect(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<
                Output = Result<crate::runtime::RuntimeSnapshot, crate::runtime::RuntimeError>,
            > + Send {
                async {
                    Err(crate::runtime::RuntimeError::NotFound(
                        "no daemon".to_string(),
                    ))
                }
            }
            fn create(
                &self,
                _spec: &crate::runtime::ValidatedServerSpec,
            ) -> impl std::future::Future<
                Output = Result<crate::runtime::RuntimeHandle, crate::runtime::RuntimeError>,
            > + Send {
                async { panic!("must not run") }
            }
            fn start(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn stop(
                &self,
                _container_id: &str,
                _timeout_secs: u32,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn restart(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn remove(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<Output = Result<(), crate::runtime::RuntimeError>> + Send
            {
                async { panic!("must not run") }
            }
            fn logs(
                &self,
                _container_id: &str,
                _tail: usize,
            ) -> impl std::future::Future<Output = Result<Vec<String>, crate::runtime::RuntimeError>>
                   + Send {
                async { panic!("must not run") }
            }
            fn stats(
                &self,
                _container_id: &str,
            ) -> impl std::future::Future<
                Output = Result<crate::runtime::RuntimeStats, crate::runtime::RuntimeError>,
            > + Send {
                async { panic!("must not run") }
            }
        }
        sample_all(&pool, Some(&Unreachable)).await;
        let rows = server_samples(&pool, "dead-1", "1970-01-01T00:00:00.000Z")
            .await
            .expect("history reads succeed");
        assert_eq!(rows.len(), 2, "one missing row per NotFound probe");
        for r in &rows {
            assert_eq!(r.state, "missing");
            assert!(
                r.cpu_percent.is_none()
                    && r.memory_bytes.is_none()
                    && r.players_online.is_none()
                    && r.latency_ms.is_none(),
                "NULLs, never zeros: {r:?}"
            );
        }
    }

    #[tokio::test]
    async fn prune_bounds_table() {
        let pool = test_pool().await;
        seed_server(&pool, "prune-1", "", 25566, "created").await;

        // 60k rows 30 days old + a few fresh rows.
        let old_ts = "2020-01-01T00:00:00.000Z";
        let mut tx = pool.begin().await.expect("tx");
        for chunk in (0..60_000).collect::<Vec<_>>().chunks(1000) {
            let mut sql = String::from(
                "INSERT INTO metrics_samples (id, server_id, sampled_at, state) VALUES ",
            );
            let mut parts = Vec::with_capacity(chunk.len());
            for i in chunk {
                parts.push(format!("('old-{i}', 'prune-1', '{old_ts}', 'exited')"));
            }
            sql.push_str(&parts.join(","));
            sqlx::query(&sql).execute(&mut *tx).await.expect("seed old");
        }
        for i in 0..5 {
            sqlx::query(
                "INSERT INTO metrics_samples (id, server_id, state) VALUES (?, 'prune-1', 'running')",
            )
            .bind(format!("fresh-{i}"))
            .execute(&mut *tx)
            .await
            .expect("seed fresh");
        }
        tx.commit().await.expect("commit");

        prune_samples(&pool).await;

        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM metrics_samples")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert!(total <= 50_000, "global cap holds, got {total}");
        let stale: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metrics_samples \
             WHERE sampled_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')",
        )
        .fetch_one(&pool)
        .await
        .expect("stale count");
        assert_eq!(stale, 0, "no rows older than 7 days survive");
        let fresh: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metrics_samples WHERE server_id = 'prune-1' AND state = 'running'",
        )
        .fetch_one(&pool)
        .await
        .expect("fresh count");
        assert_eq!(fresh, 5, "fresh rows intact");
    }

    #[tokio::test]
    async fn history_orders_and_caps() {
        let pool = test_pool().await;
        seed_server(&pool, "hist-1", "", 25567, "created").await;

        let mut tx = pool.begin().await.expect("tx");
        for i in 0..1500 {
            // Distinct, increasing timestamps (minute granularity, zero-padded).
            let ts = format!(
                "2026-09-{:02}T{:02}:{:02}:00.000Z",
                1 + (i / 1440),
                (i / 60) % 24,
                i % 60
            );
            sqlx::query(
                "INSERT INTO metrics_samples (id, server_id, sampled_at, state) \
                 VALUES (?, 'hist-1', ?, 'running')",
            )
            .bind(format!("h-{i:04}"))
            .bind(&ts)
            .execute(&mut *tx)
            .await
            .expect("seed history");
        }
        tx.commit().await.expect("commit");

        let rows = server_samples(&pool, "hist-1", "1970-01-01T00:00:00.000Z")
            .await
            .expect("history reads succeed");
        assert_eq!(rows.len(), 1000, "history capped at 1000");
        for w in rows.windows(2) {
            assert!(
                w[0].sampled_at >= w[1].sampled_at,
                "newest-first: {} before {}",
                w[0].sampled_at,
                w[1].sampled_at
            );
        }
    }

    #[tokio::test]
    async fn normalize_state_truthful_mapping() {
        assert_eq!(normalize_state("paused"), "paused");
        assert_eq!(normalize_state("weird-state"), "unknown");
    }

    #[tokio::test]
    async fn restart_count_minus_one_when_unknown() {
        let pool = test_pool().await;
        seed_server(&pool, "rs-1", "", 25568, "created").await;
        sample_all::<DockerAdapter>(&pool, None).await;
        let rc: i64 = sqlx::query_scalar(
            "SELECT restart_count FROM metrics_samples WHERE server_id = 'rs-1'",
        )
        .fetch_one(&pool)
        .await
        .expect("restart_count");
        assert_eq!(rc, -1, "unknown restart count is -1, never 0-as-unknown");
    }
}
