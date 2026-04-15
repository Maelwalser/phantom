//! Schema creation and migration logic for the event store database.
//!
//! Separated from [`crate::store`] to isolate DDL and version tracking
//! from connection management and query execution.

use sqlx::sqlite::{SqlitePool, SqliteQueryResult};

use crate::error::EventStoreError;

/// Current schema version for the event store database.
///
/// Increment this when adding migrations in [`run_migrations`].
pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 5;

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
                    Ok(SqliteQueryResult::default())
                } else {
                    Err(e)
                }
            })?;

        sqlx::query("UPDATE schema_meta SET value = '2' WHERE key = 'schema_version'")
            .execute(pool)
            .await?;
    }

    if version < 3 {
        // Migration 2 -> 3: add projection_snapshots table for snapshot-based
        // projection loading (avoids full event replay on every projection build).
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS projection_snapshots (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                snapshot_at INTEGER NOT NULL,
                data        BLOB NOT NULL,
                created_at  TEXT NOT NULL
            )",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_snapshots_at ON projection_snapshots(snapshot_at)",
        )
        .execute(pool)
        .await?;

        sqlx::query("UPDATE schema_meta SET value = '3' WHERE key = 'schema_version'")
            .execute(pool)
            .await?;
    }

    if version < 4 {
        // Migration 3 -> 4: add causal_parent column for causal DAG ordering.
        // Nullable INTEGER references the id of the event that caused this one.
        sqlx::query("ALTER TABLE events ADD COLUMN causal_parent INTEGER DEFAULT NULL")
            .execute(pool)
            .await
            .or_else(|e| {
                if e.to_string().contains("duplicate column") {
                    Ok(SqliteQueryResult::default())
                } else {
                    Err(e)
                }
            })?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_causal_parent ON events(causal_parent)")
            .execute(pool)
            .await?;

        sqlx::query("UPDATE schema_meta SET value = '4' WHERE key = 'schema_version'")
            .execute(pool)
            .await?;
    }

    if version < 5 {
        // Migration 4 -> 5: add composite indexes for common query patterns.
        // Most queries filter on `dropped = 0` first, so leading with `dropped`
        // lets SQLite skip dropped rows via the index rather than scanning.
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_events_dropped_changeset ON events(dropped, changeset_id)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_events_dropped_agent ON events(dropped, agent_id)",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_events_dropped_timestamp ON events(dropped, timestamp)",
        )
        .execute(pool)
        .await?;

        sqlx::query("UPDATE schema_meta SET value = '5' WHERE key = 'schema_version'")
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
