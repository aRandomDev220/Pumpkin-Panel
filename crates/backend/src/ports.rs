//! Server port reservations.
//!
//! A server owns its Java game port and the derived Bedrock port
//! (`game_port + 1`). Both are inserted into
//! `server_port_reservations` in the same transaction as the server row, so
//! the primary key serialises concurrent creates that would otherwise pick
//! overlapping pairs (e.g. 25565/25566 and 25566/25567).

use sqlx::{Sqlite, SqlitePool, Transaction};

/// Reserve both ports for `server_id`. Fails if either is already taken.
pub async fn reserve(
    tx: &mut Transaction<'_, Sqlite>,
    server_id: &str,
    game_port: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO server_port_reservations (port, server_id) VALUES (?, ?), (?, ?)")
        .bind(game_port)
        .bind(server_id)
        .bind(game_port + 1)
        .bind(server_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Release a server's reservations (used when it is soft-deleted).
pub async fn release(tx: &mut Transaction<'_, Sqlite>, server_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM server_port_reservations WHERE server_id = ?")
        .bind(server_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Ports currently reserved by live servers.
pub async fn taken(pool: &SqlitePool) -> Result<Vec<i64>, sqlx::Error> {
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT port FROM server_port_reservations")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|(port,)| port).collect())
}
