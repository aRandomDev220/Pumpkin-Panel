//! Daemon-backed dispatch tests (nightly-gated, `#[ignore]`d).
//!
//! Run: `cargo test -p pumpkin-manager-backend -- --ignored docker_`
//!
//! These tests talk to the real Docker daemon over `/var/run/docker.sock`
//! using the pinned `ghcr.io/pumpkin-mc/pumpkin:master` image (must be
//! present locally; nothing is pulled). When the socket is absent they
//! print `SKIP: no docker` and pass, so CI without a daemon stays green.
//!
//! Conventions (see `docs/phase3.md`): game port `26575`; Pumpkin runs as
//! UID/GID 2613, so the per-server *leaf* data dir is chowned when we run
//! as root — never the servers root, or the manager loses write access.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::runtime::{
    validate_spec, DockerAdapter, RuntimeAdapter, RuntimeError, ServerSpecInput,
    ValidatedServerSpec, DOCKER_SOCKET,
};
use crate::task_runner;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Real adapter, or `None` when there is no daemon to talk to.
///
/// Prints `SKIP: no docker` on the `None` path so the pass reason is
/// visible; callers `return` early.
fn daemon() -> Option<DockerAdapter> {
    if !Path::new(DOCKER_SOCKET).exists() {
        eprintln!("SKIP: no docker");
        return None;
    }
    match DockerAdapter::new() {
        Ok(adapter) => Some(adapter),
        Err(e) => {
            eprintln!("SKIP: no docker ({e})");
            None
        }
    }
}

/// Both tests bind the same fixed game port (`26575`), so they must not run
/// concurrently. Ignored tests share the process with the default suite but
/// only with each other here.
static DOCKER_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// In-memory pool with the real migrations (pattern from `tests_phase2.rs`).
async fn test_pool() -> SqlitePool {
    // Single connection: in-memory SQLite is per-connection.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory pool");
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .expect("pragma");
    for file in [
        include_str!("../../../migrations/0001_init.sql"),
        include_str!("../../../migrations/0002_sessions_audit.sql"),
    ] {
        for stmt in file.split(";\n") {
            let stmt = stmt.trim();
            if stmt.is_empty() || stmt.starts_with("--") && !stmt.contains("CREATE") {
                continue;
            }
            // Skip pure-comment / pragma chunks that hold no DDL.
            if !stmt.to_uppercase().contains("CREATE") && !stmt.to_uppercase().contains("INDEX") {
                continue;
            }
            sqlx::query(stmt).execute(&pool).await.expect("migration");
        }
    }
    pool
}

/// Unique `pm-docker-*` scratch root (the servers root — never chowned).
fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("pm-docker-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

/// Unique slug matching `^[a-z0-9-]{3,48}$` (also derives the container name).
fn unique_slug() -> String {
    format!("dock-{}", &uuid::Uuid::new_v4().to_string()[..8])
}

/// Validated spec for a real container: fixed game port `26575`, pinned image.
fn make_spec(server_root: &Path, slug: &str) -> ValidatedServerSpec {
    let server_id = uuid::Uuid::new_v4();
    let data_path = server_root.join(slug).to_string_lossy().into_owned();
    validate_spec(
        ServerSpecInput {
            server_id,
            name: format!("docker test {slug}"),
            slug: slug.to_string(),
            image_reference: "ghcr.io/pumpkin-mc/pumpkin:master".to_string(),
            image_digest: String::new(),
            game_port: 26575,
            bind_address: "0.0.0.0".to_string(),
            data_path,
            memory_limit_bytes: 1024 * 1024 * 1024,
            cpu_limit: 1.0,
        },
        server_root,
    )
    .expect("test spec must validate")
}

/// Ensure the per-server leaf data dir exists and — when running as root on
/// Unix — is owned by Pumpkin's UID/GID 2613. Only the leaf is ever chowned,
/// never the servers root. Non-root runs skip with a note (the image
/// entrypoint owns the production chown).
#[cfg(unix)]
fn chown_leaf_for_pumpkin(leaf: &Path) {
    std::fs::create_dir_all(leaf).expect("leaf data dir");
    let is_root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false);
    if !is_root {
        eprintln!("SKIP chown: not root; relying on image entrypoint");
        return;
    }
    if let Err(e) = std::process::Command::new("chown")
        .arg("-R")
        .arg("2613:2613")
        .arg(leaf)
        .status()
    {
        eprintln!("WARN: chown {} failed: {e}", leaf.display());
    }
}

/// Non-Unix: nothing to chown; the entrypoint owns it.
#[cfg(not(unix))]
fn chown_leaf_for_pumpkin(leaf: &Path) {
    std::fs::create_dir_all(leaf).expect("leaf data dir");
}

/// Poll `inspect` until `status == want` (1s cadence). Returns the last
/// status seen, so assertion failures show what the container actually did.
async fn wait_status(
    adapter: &DockerAdapter,
    container_id: &str,
    want: &str,
    attempts: u32,
) -> String {
    let mut last = String::from("<no inspect yet>");
    for _ in 0..attempts {
        match adapter.inspect(container_id).await {
            Ok(snap) => {
                last = snap.status.clone();
                if last == want {
                    return last;
                }
            }
            Err(e) => {
                last = format!("inspect error: {e}");
                return last;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    last
}

// ---------------------------------------------------------------------------
// Tests (all `#[ignore]`d; names start with `docker_`)
// ---------------------------------------------------------------------------

/// Full adapter cycle against the daemon: create → start → `running` →
/// stop → `exited` → remove → `NotFound`.
#[tokio::test]
#[ignore]
async fn docker_start_stop_cycle() {
    let Some(adapter) = daemon() else { return };
    let _serial = DOCKER_SERIAL.lock().await;

    let root = temp_root("cycle");
    let slug = unique_slug();
    let spec = make_spec(&root, &slug);
    chown_leaf_for_pumpkin(Path::new(&spec.data_path));

    // Inner fallible body; teardown below runs whether it passed or not.
    // (`container_id` is tracked outside so cleanup knows what to remove.)
    let mut container_id: Option<String> = None;
    let result: Result<(), String> = async {
        let handle = adapter
            .create(&spec)
            .await
            .map_err(|e| format!("create: {e}"))?;
        container_id = Some(handle.container_id.clone());
        let id = container_id.as_deref().expect("just set");

        adapter.start(id).await.map_err(|e| format!("start: {e}"))?;
        let running = wait_status(&adapter, id, "running", 60).await;
        if running != "running" {
            return Err(format!("after start: {running}"));
        }

        adapter
            .stop(id, 35)
            .await
            .map_err(|e| format!("stop: {e}"))?;
        let exited = wait_status(&adapter, id, "exited", 90).await;
        if exited != "exited" {
            return Err(format!("after stop: {exited}"));
        }

        adapter
            .remove(id)
            .await
            .map_err(|e| format!("remove: {e}"))?;
        match adapter.inspect(id).await {
            Err(RuntimeError::NotFound(_)) => Ok(()),
            Ok(snap) => Err(format!(
                "expected NotFound after remove, got {}",
                snap.status
            )),
            Err(e) => Err(format!("expected NotFound after remove, got {e}")),
        }
    }
    .await;

    // Defer-style teardown: always runs, even on failure.
    if let Some(id) = container_id.as_deref() {
        let _ = adapter.stop(id, 10).await;
        let _ = adapter.remove(id).await;
    }
    let _ = std::fs::remove_dir_all(&root);

    result.expect("start/stop cycle");
}

/// `dispatch_task` with the real adapter: an enqueued `start` task succeeds
/// and syncs `servers.observed_state` to `"running"`.
#[tokio::test]
#[ignore]
async fn docker_dispatch_start_updates_observed() {
    let Some(adapter) = daemon() else { return };
    let _serial = DOCKER_SERIAL.lock().await;

    let pool = test_pool().await;
    let root = temp_root("dispatch");
    let slug = unique_slug();
    let spec = make_spec(&root, &slug);
    chown_leaf_for_pumpkin(Path::new(&spec.data_path));

    // Inner fallible body; teardown below runs whether setup,
    // dispatch, or assertions fail. (`container_id` is tracked outside
    // so cleanup knows what to remove.)
    let server_id = spec.server_id.to_string();
    let mut container_id: Option<String> = None;
    let result: Result<String, String> = async {
        let handle = adapter
            .create(&spec)
            .await
            .map_err(|e| format!("create container for dispatch: {e}"))?;
        container_id = Some(handle.container_id.clone());

        sqlx::query(
            "INSERT INTO servers (id, name, slug, image_reference, container_id, data_path, game_port, observed_state)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'created')",
        )
        .bind(&server_id)
        .bind(&spec.name)
        .bind(&spec.slug)
        .bind(&spec.image_reference)
        .bind(&handle.container_id)
        .bind(&spec.data_path)
        .bind(i64::from(spec.game_port))
        .execute(&pool)
        .await
        .map_err(|e| format!("server row: {e}"))?;

        let task = task_runner::enqueue(&pool, "start", Some(&server_id), None)
            .await
            .map_err(|e| format!("enqueue start: {e}"))?;
        let task_id = task.id.clone();

        task_runner::dispatch_task(&pool, &adapter, &task.id)
            .await
            .map_err(|e| format!("dispatch start: {e}"))?;
        Ok(task_id)
    }
    .await;

    // Defer-style teardown: always runs, even on early failure.
    if let Some(id) = container_id.as_deref() {
        let _ = adapter.stop(id, 10).await;
        let _ = adapter.remove(id).await;
    }
    let _ = std::fs::remove_dir_all(&root);

    let task_id = result.expect("dispatch start");
    let rec = task_runner::get(&pool, &task_id)
        .await
        .expect("get task")
        .expect("task row");
    assert_eq!(
        rec.state,
        task_runner::TaskState::Succeeded,
        "task error: {:?} {:?}",
        rec.error_code,
        rec.error_summary
    );
    let observed: String = sqlx::query_scalar("SELECT observed_state FROM servers WHERE id = ?")
        .bind(&server_id)
        .fetch_one(&pool)
        .await
        .expect("observed state");
    assert_eq!(observed, "running");
}
