//! Audit event writer (spec section 12.6).
//!
//! Sensitive actions record actor/action/target/outcome/request id plus
//! redacted metadata. Never pass passwords, tokens, or file contents in
//! `metadata`.

use sqlx::SqlitePool;

/// Write one audit row. Best-effort: failures are logged, never fatal.
#[allow(clippy::too_many_arguments)] // fixed audit schema: one arg per column
pub async fn record(
    pool: &SqlitePool,
    actor_user_id: Option<&str>,
    action: &str,
    target_type: &str,
    target_id: &str,
    outcome: &str,
    request_id: &str,
    metadata: serde_json::Value,
) {
    let id = uuid::Uuid::new_v4().to_string();
    let metadata_str = metadata.to_string();
    if let Err(e) = sqlx::query(
        "INSERT INTO audit_events
            (id, actor_user_id, action, target_type, target_id, outcome, request_id, metadata)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(actor_user_id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(outcome)
    .bind(request_id)
    .bind(&metadata_str)
    .execute(pool)
    .await
    {
        tracing::warn!(%action, error = %e, "audit write failed");
    }
}

/// Fresh request id for handlers without Tower request-id middleware yet.
pub fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
