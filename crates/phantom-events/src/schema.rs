//! Schema creation and migration logic for the event store database.
//!
//! Separated from [`crate::store`] to isolate DDL and version tracking
//! from connection management and query execution.

use sqlx::sqlite::SqlitePool;

use crate::error::EventStoreError;

/// Current schema version for the event store database.
///
/// Increment this when adding migrations in [`run_migrations`].
pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Create the events table, schema_meta table, and indexes if they do not exist.
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

/// Read the current schema version from the database.
pub(crate) async fn schema_version(pool: &SqlitePool) -> Result<u32, EventStoreError> {
    let row: (String,) =
        sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
            .fetch_one(pool)
            .await?;
    row.0.parse().map_err(|_| EventStoreError::SchemaMismatch {
        expected: CURRENT_SCHEMA_VERSION,
        found: 0,
    })
}

/// Run forward migrations up to [`CURRENT_SCHEMA_VERSION`].
pub(crate) async fn run_migrations(pool: &SqlitePool) -> Result<(), EventStoreError> {
    let version = schema_version(pool).await?;

    if version < 2 {
        // Migration 1 -> 2: add kind_version column for envelope versioning.
        sqlx::query("ALTER TABLE events ADD COLUMN kind_version INTEGER NOT NULL DEFAULT 1")
            .execute(pool)
            .await
            // Column may already exist if a previous migration was interrupted
            // after the ALTER but before the version update.
            .or_else(|e| {
                if e.to_string().contains("duplicate column") {
                    Ok(Default::default())
                } else {
                    Err(e)
                }
            })?;

        sqlx::query("UPDATE schema_meta SET value = '2' WHERE key = 'schema_version'")
            .execute(pool)
            .await?;
    }

    if version > CURRENT_SCHEMA_VERSION {
        return Err(EventStoreError::SchemaMismatch {
            expected: CURRENT_SCHEMA_VERSION,
            found: version,
        });
    }

    Ok(())
}