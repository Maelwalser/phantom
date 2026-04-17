//! Initial schema creation — idempotent CREATE TABLE/INDEX statements.
//!
//! Only covers the v1 baseline: the `schema_meta` tracking table, the
//! `events` table, and its single-column indexes. Version-specific columns
//! and indexes are applied by [`super::migrations`].

use sqlx::sqlite::SqlitePool;

use crate::error::EventStoreError;

/// Create the events table, schema_meta table, and v1 indexes if they do
/// not exist. Idempotent — safe to call on every open.
pub(crate) async fn ensure_schema(pool: &SqlitePool) -> Result<(), EventStoreError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    // Seed initial schema version if not present.
    sqlx::query("INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('schema_version', '1')")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS events (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp    TEXT NOT NULL,
            changeset_id TEXT NOT NULL,
            agent_id     TEXT NOT NULL,
            kind         TEXT NOT NULL,
            dropped      INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_changeset ON events(changeset_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_agent ON events(agent_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp)")
        .execute(pool)
        .await?;

    Ok(())
}
