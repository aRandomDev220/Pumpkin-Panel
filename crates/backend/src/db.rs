use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::time::Duration;

pub async fn connect(database_url: &str) -> anyhow::Result<sqlx::SqlitePool> {
    // Allow `sqlite:/path` and `sqlite://` forms.
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        // Concurrent owner/port guards wait for the write lock instead of
        // surfacing SQLITE_BUSY as a 500.
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}

/// Delete expired/revoked sessions and audit rows older than 90 days.
/// Called from the scheduler loop so both tables stop growing forever.
pub async fn prune_ephemeral(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM sessions
         WHERE expires_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
            OR (revoked_at IS NOT NULL
                AND revoked_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 day'))",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "DELETE FROM audit_events
         WHERE created_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-90 days')",
    )
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Migration / schema-integrity tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row as _;

    /// Fresh file-backed pool whose schema comes from the *real* embedded
    /// migration set (so `0008_integrity.sql` is exercised exactly as on
    /// boot, including its transaction handling).
    async fn migrated_pool() -> (std::path::PathBuf, sqlx::SqlitePool) {
        let dir = std::env::temp_dir().join(format!("pm-mig-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("pumpkin-manager.db");
        let url = format!("sqlite://{}", path.display());
        let pool = connect(&url).await.expect("connect + migrate");
        (dir, pool)
    }

    /// FK enforcement must be on for this test to mean anything.
    #[tokio::test]
    async fn delete_user_nulls_fk_and_succeeds() {
        let (dir, pool) = migrated_pool().await;

        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .expect("pragma");
        assert_eq!(fk, 1, "foreign_keys must be enforced");

        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role) VALUES ('u1','u1','h','owner')",
        )
        .execute(&pool)
        .await
        .expect("user");
        sqlx::query(
            "INSERT INTO servers (id, name, slug, game_port, created_by) VALUES ('s1','srv','srv',25560,'u1')",
        )
        .execute(&pool)
        .await
        .expect("server");
        sqlx::query(
            "INSERT INTO tasks (id, type, server_id, requested_by, state) VALUES ('t1','start','s1','u1','queued')",
        )
        .execute(&pool)
        .await
        .expect("task");
        sqlx::query("INSERT INTO audit_events (id, actor_user_id, action) VALUES ('a1','u1','x')")
            .execute(&pool)
            .await
            .expect("audit");

        // With NO ACTION (pre-migration) FKs this fails; provenance FKs must
        // be ON DELETE SET NULL so the account can be removed.
        sqlx::query("DELETE FROM users WHERE id = 'u1'")
            .execute(&pool)
            .await
            .expect("delete user must succeed with SET NULL provenance FKs");

        let created_by: Option<String> =
            sqlx::query_scalar("SELECT created_by FROM servers WHERE id = 's1'")
                .fetch_one(&pool)
                .await
                .expect("server row");
        let requested_by: Option<String> =
            sqlx::query_scalar("SELECT requested_by FROM tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .expect("task row");
        let actor_user_id: Option<String> =
            sqlx::query_scalar("SELECT actor_user_id FROM audit_events WHERE id = 'a1'")
                .fetch_one(&pool)
                .await
                .expect("audit row");

        assert_eq!(created_by, None, "servers.created_by not nulled");
        assert_eq!(requested_by, None, "tasks.requested_by not nulled");
        assert_eq!(actor_user_id, None, "audit_events.actor_user_id not nulled");

        // Referencing rows survive (they were nulled, not cascaded).
        let servers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers")
            .fetch_one(&pool)
            .await
            .expect("count");
        let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(servers, 1);
        assert_eq!(tasks, 1);

        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn soft_deleted_server_frees_name_slug_port() {
        let (dir, pool) = migrated_pool().await;

        sqlx::query(
            "INSERT INTO servers (id, name, slug, game_port) VALUES ('s1','alpha','alpha',25561)",
        )
        .execute(&pool)
        .await
        .expect("first server");
        sqlx::query(
            "UPDATE servers SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = 's1'",
        )
        .execute(&pool)
        .await
        .expect("soft delete");

        // Table-level UNIQUE would reject this; partial indexes scoped to
        // `deleted_at IS NULL` must let the identity be reused.
        sqlx::query(
            "INSERT INTO servers (id, name, slug, game_port) VALUES ('s2','alpha','alpha',25561)",
        )
        .execute(&pool)
        .await
        .expect("reuse after soft-delete must succeed");

        let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM servers WHERE deleted_at IS NULL")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(live, 1);

        // A live collision on any one of name/slug/port must still be rejected.
        let collisions = [
            ("s3", 'n', "INSERT INTO servers (id, name, slug, game_port) VALUES ('s3','alpha','other',25563)"),
            ("s4", 's', "INSERT INTO servers (id, name, slug, game_port) VALUES ('s4','other','alpha',25564)"),
            ("s5", 'p', "INSERT INTO servers (id, name, slug, game_port) VALUES ('s5','other','other',25561)"),
        ];
        for (id, kind, sql) in collisions {
            let res = sqlx::query(sql).execute(&pool).await;
            assert!(
                res.is_err(),
                "live duplicate on {kind} ({id}) must still be rejected"
            );
        }

        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Migrating a *populated* previous-version database must not lose rows.
    /// This is the real hazard: without `PRAGMA foreign_keys=OFF` the
    /// `DROP TABLE servers` inside the rebuild fires the children's
    /// `ON DELETE CASCADE` actions.
    #[tokio::test]
    async fn migration_0008_preserves_existing_rows() {
        let dir = std::env::temp_dir().join(format!("pm-mig7-{}", uuid::Uuid::new_v4()));
        let migdir = dir.join("migrations");
        std::fs::create_dir_all(&migdir).expect("migration dir");
        let v7: [(&str, &str); 7] = [
            (
                "0001_init.sql",
                include_str!("../../../migrations/0001_init.sql"),
            ),
            (
                "0002_sessions_audit.sql",
                include_str!("../../../migrations/0002_sessions_audit.sql"),
            ),
            (
                "0003_config_revisions.sql",
                include_str!("../../../migrations/0003_config_revisions.sql"),
            ),
            (
                "0004_config_baseline_hash.sql",
                include_str!("../../../migrations/0004_config_baseline_hash.sql"),
            ),
            (
                "0005_backups.sql",
                include_str!("../../../migrations/0005_backups.sql"),
            ),
            (
                "0006_metrics.sql",
                include_str!("../../../migrations/0006_metrics.sql"),
            ),
            (
                "0007_update.sql",
                include_str!("../../../migrations/0007_update.sql"),
            ),
        ];
        for (name, sql) in v7 {
            std::fs::write(migdir.join(name), sql).expect("write migration");
        }

        let path = dir.join("pumpkin-manager.db");
        let url = format!("sqlite://{}", path.display());
        let opts = SqliteConnectOptions::from_str(&url)
            .expect("url")
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("pool");

        // Bring the DB to the previous released schema, then populate it.
        sqlx::migrate::Migrator::new(migdir.as_path())
            .await
            .expect("v7 migrator")
            .run(&pool)
            .await
            .expect("apply v7");

        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role) VALUES ('u1','u1','h','owner')",
        )
        .execute(&pool)
        .await
        .expect("user");
        sqlx::query("INSERT INTO servers (id, name, slug, game_port, created_by) VALUES ('s1','srv','srv',25562,'u1')")
            .execute(&pool)
            .await
            .expect("server");
        sqlx::query("INSERT INTO server_settings (server_id) VALUES ('s1')")
            .execute(&pool)
            .await
            .expect("settings");
        sqlx::query("INSERT INTO tasks (id, type, server_id, requested_by, state) VALUES ('t1','start','s1','u1','succeeded')")
            .execute(&pool)
            .await
            .expect("task");
        sqlx::query("INSERT INTO backups (id, server_id, created_by) VALUES ('b1','s1','u1')")
            .execute(&pool)
            .await
            .expect("backup");
        sqlx::query(
            "INSERT INTO backup_policies (id, server_id, created_by) VALUES ('p1','s1','u1')",
        )
        .execute(&pool)
        .await
        .expect("policy");
        sqlx::query("INSERT INTO scheduled_tasks (id, server_id, kind, created_by) VALUES ('st1','s1','restart','u1')")
            .execute(&pool)
            .await
            .expect("scheduled");
        sqlx::query("INSERT INTO config_revisions (id, server_id, revision, content, hash, created_by) VALUES ('cr1','s1',1,'{}','h','u1')")
            .execute(&pool)
            .await
            .expect("revision");
        sqlx::query("INSERT INTO metrics_samples (id, server_id) VALUES ('m1','s1')")
            .execute(&pool)
            .await
            .expect("metric");

        // Apply 0008 with the real embedded migration set.
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("apply 0008");

        // No row may have been cascaded away by the rebuild.
        for (table, id_col, id) in [
            ("servers", "id", "s1"),
            ("server_settings", "server_id", "s1"),
            ("tasks", "id", "t1"),
            ("backups", "id", "b1"),
            ("backup_policies", "id", "p1"),
            ("scheduled_tasks", "id", "st1"),
            ("config_revisions", "id", "cr1"),
            ("metrics_samples", "id", "m1"),
        ] {
            let n: i64 =
                sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {id_col} = ?"))
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .expect("count");
            assert_eq!(n, 1, "{table} row lost during rebuild");
        }

        // The rebuilt schema must be internally consistent.
        let violations = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .expect("foreign_key_check");
        assert!(
            violations.is_empty(),
            "foreign_key_check reported {} violation(s)",
            violations.len()
        );

        // And the new FK action is live: deleting the user nulls, not cascades.
        sqlx::query("DELETE FROM users WHERE id = 'u1'")
            .execute(&pool)
            .await
            .expect("delete user");
        let requested_by: Option<String> =
            sqlx::query_scalar("SELECT requested_by FROM tasks WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .expect("task");
        assert_eq!(requested_by, None);

        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Both the Java port and the derived Bedrock port are reserved, and the
    /// reservation primary key rejects an adjacent pair. Releasing (as the
    /// delete path does) makes the pair reusable.
    #[tokio::test]
    async fn adjacent_ports_are_reserved_and_released() {
        let (dir, pool) = migrated_pool().await;

        sqlx::query(
            "INSERT INTO servers (id, name, slug, game_port) VALUES ('s1','one','one',25565)",
        )
        .execute(&pool)
        .await
        .expect("first server");
        sqlx::query(
            "INSERT INTO servers (id, name, slug, game_port) VALUES ('s2','two','two',25566)",
        )
        .execute(&pool)
        .await
        .expect("second server");

        let mut tx = pool.begin().await.expect("tx");
        crate::ports::reserve(&mut tx, "s1", 25565)
            .await
            .expect("reserve s1");
        tx.commit().await.expect("commit");

        let ports: Vec<i64> = sqlx::query_scalar(
            "SELECT port FROM server_port_reservations WHERE server_id = 's1' ORDER BY port",
        )
        .fetch_all(&pool)
        .await
        .expect("reservations");
        assert_eq!(ports, vec![25565, 25566]);

        // s2's game port is s1's Bedrock port.
        let mut tx = pool.begin().await.expect("tx");
        let err = crate::ports::reserve(&mut tx, "s2", 25566)
            .await
            .expect_err("adjacent port must collide");
        assert!(
            err.to_string().contains("server_port_reservations"),
            "unexpected error: {err}"
        );
        tx.rollback().await.expect("rollback");

        let mut tx = pool.begin().await.expect("tx");
        crate::ports::release(&mut tx, "s1").await.expect("release");
        crate::ports::reserve(&mut tx, "s2", 25566)
            .await
            .expect("port is free after release");
        tx.commit().await.expect("commit");

        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The plan's index list must exist after migration.
    #[tokio::test]
    async fn integrity_indexes_present() {
        let (dir, pool) = migrated_pool().await;
        let rows = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'index'")
            .fetch_all(&pool)
            .await
            .expect("indexes");
        let names: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
        for want in [
            "idx_sessions_expires",
            "idx_tasks_server_state",
            "idx_tasks_state_created",
            "idx_backups_server_created",
            "idx_config_revisions_server_created",
            "idx_servers_name_active",
            "idx_servers_slug_active",
            "idx_servers_game_port_active",
        ] {
            assert!(names.iter().any(|n| n == want), "missing index {want}");
        }
        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[tokio::test]
    async fn prune_ephemeral_removes_expired_sessions_and_old_audit() {
        let (dir, pool) = migrated_pool().await;
        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role) VALUES ('u1','u1','h','owner')",
        )
        .execute(&pool)
        .await
        .expect("user");
        let recent = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        for (id, expires, revoked) in [
            ("expired", "2000-01-01T00:00:00.000Z", None),
            ("valid", "2099-01-01T00:00:00.000Z", None),
            (
                "revoked-old",
                "2099-01-01T00:00:00.000Z",
                Some("2000-01-01T00:00:00.000Z"),
            ),
            (
                "revoked-recent",
                "2099-01-01T00:00:00.000Z",
                Some(recent.as_str()),
            ),
        ] {
            sqlx::query(
                "INSERT INTO sessions (id, user_id, token_hash, expires_at, revoked_at)
                 VALUES (?, 'u1', ?, ?, ?)",
            )
            .bind(id)
            .bind(format!("hash-{id}"))
            .bind(expires)
            .bind(revoked)
            .execute(&pool)
            .await
            .expect("session");
        }
        sqlx::query(
            "INSERT INTO audit_events (id, action, target_type, target_id, outcome, request_id, created_at)
             VALUES ('old','x','y','z','ok','r','2020-01-01T00:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .expect("old audit");
        sqlx::query(
            "INSERT INTO audit_events (id, action, target_type, target_id, outcome, request_id)
             VALUES ('recent','x','y','z','ok','r')",
        )
        .execute(&pool)
        .await
        .expect("recent audit");

        prune_ephemeral(&pool).await.expect("prune");

        let sessions: Vec<String> = sqlx::query_scalar("SELECT id FROM sessions ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("sessions");
        assert_eq!(sessions, vec!["revoked-recent", "valid"]);
        let audits: Vec<String> = sqlx::query_scalar("SELECT id FROM audit_events ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("audit");
        assert_eq!(audits, vec!["recent"]);

        drop(pool);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
