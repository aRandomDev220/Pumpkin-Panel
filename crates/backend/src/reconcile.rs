//! Desired-state reconciliation.
//!
//! The manager owns restarts (`RestartPolicy: no` on child containers), so
//! something must compare `desired_state` with Docker reality and act:
//!
//! * `desired_state = running`, container exited/missing → start or recreate.
//! * `desired_state = stopped`, container still running → stop.
//!
//! `tick` also refreshes `observed_state`, so status reads stop lying between
//! explicit user operations. Enqueued actions run through the normal task
//! runner (persisted rows, concurrency cap, audit), and a per-server backoff
//! prevents a crash loop when a server cannot start.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use tracing::{info, warn};

use crate::runtime::{DockerAdapter, RuntimeAdapter, RuntimeError};
use crate::task_runner::{self, RunnerHandle};

/// First retry delay after an action is enqueued.
const BASE_BACKOFF: Duration = Duration::from_secs(30);
/// Cap for the exponential retry delay.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// One reconciliation action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Container exists but is not running.
    Start,
    /// Container exists and is running but the operator wants it stopped.
    Stop,
    /// Container is gone or was never materialised.
    Recreate,
}

/// Decide what (if anything) to do for one server.
#[must_use]
pub fn decide(desired: &str, observed: &str, container_id: &str) -> Option<Action> {
    match desired {
        "running" => match observed {
            "running" | "starting" => None,
            "missing" => Some(Action::Recreate),
            "exited" | "created" => {
                if container_id.is_empty() {
                    Some(Action::Recreate)
                } else {
                    Some(Action::Start)
                }
            }
            // A stop is in flight; conflicting_task blocks, and the next tick
            // will see `exited` and start it if it is still desired.
            "stopping" => None,
            // Unknown/unreachable: never act on a guess.
            _ => None,
        },
        "stopped" => match observed {
            "running" | "starting" => Some(Action::Stop),
            _ => None,
        },
        _ => None,
    }
}

/// Map a Docker container status to the manager's vocabulary.
fn map_status(status: &str) -> String {
    match status {
        "running" => "running",
        "created" => "created",
        "restarting" => "starting",
        "removing" => "stopping",
        _ => "exited",
    }
    .to_string()
}

struct RetryState {
    attempts: u32,
    next_at: Instant,
}

/// Reconciliation loop state (retry/backoff bookkeeping).
#[derive(Default)]
pub struct Reconciler {
    retry: HashMap<String, RetryState>,
}

impl Reconciler {
    /// One reconciliation pass. Safe to call every 60 s (and once at boot).
    pub async fn tick(
        &mut self,
        pool: &SqlitePool,
        adapter: Option<&DockerAdapter>,
        runner: Option<&RunnerHandle>,
    ) {
        let (Some(adapter), Some(runner)) = (adapter, runner) else {
            // No runtime means no action is possible; inventory endpoints
            // already report the degraded state.
            return;
        };
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT id, container_id, desired_state
             FROM servers WHERE deleted_at IS NULL",
        )
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        let now = Instant::now();
        for (server_id, container_id, desired) in rows {
            if !self.retry_allowed(&server_id, now) {
                continue;
            }
            if conflicting_task(pool, &server_id).await {
                continue;
            }

            let observed = if container_id.is_empty() {
                "missing".to_string()
            } else {
                match adapter.inspect(&container_id).await {
                    Ok(snapshot) => map_status(&snapshot.status),
                    Err(RuntimeError::NotFound(_)) => "missing".to_string(),
                    // Daemon/transient error: leave the stored observation
                    // alone rather than persisting a guess.
                    Err(_) => continue,
                }
            };
            let _ = sqlx::query(
                "UPDATE servers SET observed_state = ?,
                        updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?",
            )
            .bind(&observed)
            .bind(&server_id)
            .execute(pool)
            .await;

            let Some(action) = decide(&desired, &observed, &container_id) else {
                self.retry.remove(&server_id);
                continue;
            };
            let task_type = match action {
                Action::Start => "start",
                Action::Stop => "stop",
                Action::Recreate => "recreate",
            };
            match task_runner::enqueue_and_submit(pool, runner, task_type, Some(&server_id), None)
                .await
            {
                Ok(task) => {
                    info!(
                        server_id = %server_id,
                        action = task_type,
                        task_id = %task.id,
                        "reconciled desired state"
                    );
                }
                Err(e) => warn!(
                    server_id = %server_id,
                    action = task_type,
                    error = %e,
                    "reconcile action could not be queued"
                ),
            }
            self.schedule_retry(&server_id, now);
        }
    }

    fn retry_allowed(&self, server_id: &str, now: Instant) -> bool {
        self.retry
            .get(server_id)
            .is_none_or(|state| now >= state.next_at)
    }

    fn schedule_retry(&mut self, server_id: &str, now: Instant) {
        let state = self
            .retry
            .entry(server_id.to_string())
            .or_insert(RetryState {
                attempts: 0,
                next_at: now,
            });
        state.attempts = state.attempts.saturating_add(1);
        let factor = 1u64 << state.attempts.min(5);
        let delay = Duration::from_secs(
            BASE_BACKOFF
                .as_secs()
                .saturating_mul(factor)
                .min(MAX_BACKOFF.as_secs()),
        );
        state.next_at = now + delay;
    }
}

/// A queued/running task already owns this server.
async fn conflicting_task(pool: &SqlitePool, server_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tasks
         WHERE server_id = ? AND state IN ('queued', 'running')",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
        > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_start_is_retried_instead_of_reported_already() {
        // Leftover from a failed start: desired=running, observed=exited.
        assert_eq!(decide("running", "exited", "abc"), Some(Action::Start));
    }

    #[test]
    fn running_server_is_left_alone() {
        assert_eq!(decide("running", "running", "abc"), None);
        assert_eq!(decide("running", "starting", "abc"), None);
    }

    #[test]
    fn missing_container_is_recreated() {
        assert_eq!(decide("running", "missing", "abc"), Some(Action::Recreate));
        assert_eq!(decide("running", "exited", ""), Some(Action::Recreate));
    }

    #[test]
    fn stopped_desired_stops_a_running_container() {
        assert_eq!(decide("stopped", "running", "abc"), Some(Action::Stop));
        assert_eq!(decide("stopped", "starting", "abc"), Some(Action::Stop));
    }

    #[test]
    fn stopped_server_is_left_alone() {
        assert_eq!(decide("stopped", "exited", "abc"), None);
        assert_eq!(decide("stopped", "missing", "abc"), None);
    }

    #[test]
    fn unknown_state_is_never_acted_on() {
        assert_eq!(decide("running", "unreachable", "abc"), None);
        assert_eq!(decide("running", "stopping", "abc"), None);
        assert_eq!(decide("paused", "running", "abc"), None);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let mut r = Reconciler::default();
        let now = Instant::now();
        let mut delays = Vec::new();
        for _ in 0..8 {
            r.schedule_retry("s", now);
            delays.push(r.retry["s"].next_at.duration_since(now));
        }
        assert_eq!(delays[0], Duration::from_secs(60)); // 30 * 2^1
        assert!(delays.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(*delays.last().unwrap(), MAX_BACKOFF);
    }
}
