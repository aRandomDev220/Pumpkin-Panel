//! Host + server metrics summary (Phase 3).
//!
//! Honest numbers only: host CPU/memory/disk from the OS, per-server
//! CPU/memory from one-shot Docker stats. Anything unreachable is an
//! explicit `null`, never a zero masquerading as idle. No TPS inference,
//! no player counts (those come from the readiness ping, not here).

use std::sync::Arc;

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Json,
};

use crate::{auth::AuthUser, runtime::RuntimeAdapter, AppState};

/// GET /api/v1/metrics/summary — host stats + per-server resource views.
pub async fn metrics_summary(State(state): State<AppState>, _user: AuthUser) -> Response {
    let host = host_stats().await;
    let mut servers = Vec::new();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, name, container_id FROM servers WHERE deleted_at IS NULL ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for (id, name, container_id) in rows {
        let (cpu, mem, mem_limit, container_state) = match (&state.adapter, container_id.as_str()) {
            (Some(adapter), cid) if !cid.is_empty() => match adapter.stats(cid).await {
                Ok(s) => (
                    Some(s.cpu_percent),
                    Some(s.memory_bytes),
                    Some(s.memory_limit),
                    state_of(adapter, cid).await,
                ),
                Err(_) => (None, None, None, None),
            },
            _ => (None, None, None, None),
        };
        servers.push(serde_json::json!({
            "server_id": id,
            "name": name,
            "cpu_percent": cpu,
            "memory_bytes": mem,
            "memory_limit": mem_limit,
            "state": container_state,
        }));
    }
    Json(serde_json::json!({ "host": host, "servers": servers })).into_response()
}

async fn state_of(
    adapter: &Arc<crate::runtime::DockerAdapter>,
    container_id: &str,
) -> Option<String> {
    adapter
        .inspect(container_id)
        .await
        .ok()
        .map(|snap| snap.status)
}

/// Host CPU/memory/disk snapshot. CPU% needs two samples (`MINIMUM_CPU_
/// UPDATE_INTERVAL`); a single 250 ms settle keeps one HTTP call truthful.
async fn host_stats() -> serde_json::Value {
    // `spawn_blocking`: sysinfo refresh can block the async runtime.
    tokio::task::spawn_blocking(|| {
        use sysinfo::{Disks, System};
        let mut sys = System::new();
        sys.refresh_cpu_all();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_cpu_all();
        sys.refresh_memory();
        let cpu_percent = sys.global_cpu_usage() as f64;
        let disks = Disks::new_with_refreshed_list();
        let root = disks
            .iter()
            .find(|d| d.mount_point().to_string_lossy() == "/")
            .or_else(|| disks.first());
        let (disk_total, disk_free) = root
            .map(|d| (d.total_space(), d.available_space()))
            .unwrap_or((0, 0));
        serde_json::json!({
            "cpu_percent": (cpu_percent * 10.0).round() / 10.0,
            "memory_total_bytes": sys.total_memory(),
            "memory_used_bytes": sys.used_memory(),
            "disk_total_bytes": disk_total,
            "disk_free_bytes": disk_free,
        })
    })
    .await
    .unwrap_or(serde_json::json!({
        "cpu_percent": null,
        "memory_total_bytes": 0,
        "memory_used_bytes": 0,
        "disk_total_bytes": 0,
        "disk_free_bytes": 0,
    }))
}
