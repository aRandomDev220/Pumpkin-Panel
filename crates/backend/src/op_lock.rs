//! RAII per-server operation locks (audit remediation Task 1).
//!
//! `AppState::op_locks` is one process-wide set of server ids that currently
//! have an exclusive operation (lifecycle, config save, backup/restore,
//! image update) in flight. Handlers use [`try_acquire`] to fail fast with a
//! busy response, or [`acquire`] to wait their turn.
//!
//! Dropping the returned [`OpGuard`] releases the lock. `Drop` cannot await,
//! so the actual removal is *deferred* onto the Tokio runtime; [`acquire`]
//! therefore loops until it observes the freed slot. The lock set type is
//! [`OpLocks`], which is identical to `AppState::op_locks`, so this module is
//! a drop-in replacement for the local helpers it supersedes.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::Mutex;

/// Shared set of server ids with an exclusive operation in flight.
///
/// This is the exact type of `AppState::op_locks`; the alias exists so
/// callers and tests never restate the (deliberately frozen) concrete type.
pub type OpLocks = Arc<Mutex<HashSet<String>>>;

/// RAII guard proving the key in `self.key` is held. Dropping releases it.
#[derive(Debug)]
pub struct OpGuard {
    locks: OpLocks,
    key: String,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        let locks = Arc::clone(&self.locks);
        let key = std::mem::take(&mut self.key);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // Deferred: `Drop` cannot await, so the removal runs as its own
            // task. Callers that need the lock immediately use [`acquire`],
            // which loops until the slot is actually free.
            handle.spawn(async move {
                locks.lock().await.remove(&key);
            });
        } else if let Ok(mut set) = self.locks.try_lock() {
            // No runtime (e.g. process teardown): release synchronously when
            // the mutex is uncontended, which it is outside async execution.
            set.remove(&key);
        }
        // Otherwise the guard leaks its key; this only happens if a guard is
        // dropped outside the runtime while another holder is mid-release, an
        // impossible interleaving in this process model. `acquire` would then
        // block; that failure mode is loud, never silent corruption.
    }
}

/// Try to claim `server_id` without waiting. `None` means another operation
/// holds it.
pub async fn try_acquire(locks: &OpLocks, server_id: &str) -> Option<OpGuard> {
    let mut set = locks.lock().await;
    if set.contains(server_id) {
        return None;
    }
    set.insert(server_id.to_string());
    drop(set);
    Some(OpGuard {
        locks: Arc::clone(locks),
        key: server_id.to_string(),
    })
}

/// Wait until `server_id` is free, then claim it.
///
/// Loops because the previous holder's `Drop` releases the key on a spawned
/// task, so the slot may not be free the instant this is called.
pub async fn acquire(locks: &OpLocks, server_id: &str) -> OpGuard {
    loop {
        if let Some(guard) = try_acquire(locks, server_id).await {
            return guard;
        }
        // Bounded backoff: an op may legitimately hold the lock for seconds
        // (backup/restore), so do not spin.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locks() -> OpLocks {
        OpLocks::default()
    }

    #[tokio::test]
    async fn op_lock_mutual_exclusion() {
        let locks = locks();
        let _first = try_acquire(&locks, "srv-1").await.expect("first acquire");
        assert!(
            try_acquire(&locks, "srv-1").await.is_none(),
            "same key must be excluded while held"
        );
        // A different key is independent.
        let _other = try_acquire(&locks, "srv-2")
            .await
            .expect("different key is free");
    }

    #[tokio::test]
    async fn op_lock_released_on_drop() {
        let locks = locks();
        let guard = try_acquire(&locks, "srv-1").await.expect("first acquire");
        drop(guard);

        // Removal is deferred, so `acquire` must wait for the spawned task.
        let again =
            tokio::time::timeout(std::time::Duration::from_secs(2), acquire(&locks, "srv-1"))
                .await
                .expect("lock must be released on drop");
        drop(again);

        // And it is released again after the second guard drops.
        let third =
            tokio::time::timeout(std::time::Duration::from_secs(2), acquire(&locks, "srv-1"))
                .await
                .expect("lock must be released again");
        // `try_acquire` from a concurrent holder must still fail while held.
        assert!(try_acquire(&locks, "srv-1").await.is_none());
        drop(third);
    }
}
