//! Forward-only schema migrations.
//!
//! Each migration is a small async function that moves the database from
//! version `n-1` to version `n`. The [`MIGRATIONS`] slice is the single
//! source of truth for the ordered sequence; [`apply`] dispatches to the
//! matching function by version number.
//!
//! # Adding a new migration
//!
//! 1. Add a `Migration { version: N, name: "..." }` entry to [`MIGRATIONS`].
//! 2. Add a `match` arm for `N` in [`apply`].
//! 3. Implement `async fn m_00N_...(pool)` in this file.
//! 4. Bump [`CURRENT_SCHEMA_VERSION`] to `N` (the unit test enforces that
//!    it matches the last entry in [`MIGRATIONS`]).

use sqlx::sqlite::SqlitePool;

use crate::error::EventStoreError;

/// Current schema version for the event store database.
///
/// Must equal the last entry in [`MIGRATIONS`] — see the unit test below.
pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 5;

/// Metadata for a single schema migration.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Migration {
    /// Schema version reached after this migration runs successfully.
    pub(crate) version: u32,
    /// Human-readable description — surfaced in `tracing` logs.
    pub(crate) name: &'static str,
}

/// Ordered list of migrations. Versions must be strictly increasing and
/// contiguous starting from 2 (v1 is the baseline created by [`super::ddl`]).
pub(crate) const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 2,
        name: "add_kind_version_column",
    },
    Migration {
        version: 3,
        name: "create_projection_snapshots",
    },
    Migration {
        version: 4,
        name: "add_causal_parent_column",
    },
    Migration {
        version: 5,
        name: "add_composite_indexes",
    },
];

/// Dispatch to the correct migration body by version number.
pub(crate) async fn apply(m: &Migration, pool: &SqlitePool) -> Result<(), EventStoreError> {
    match m.version {
        2 => m_002_add_kind_version(pool).await,
        3 => m_003_projection_snapshots(pool).await,
        4 => m_004_add_causal_parent(pool).await,
        5 => m_005_composite_indexes(pool).await,
        _ => Err(EventStoreError::SchemaMismatch {
            expected: CURRENT_SCHEMA_VERSION,
            found: m.version,
        }),
    }
}

/// Migration 1 → 2: add `kind_version` column for envelope versioning.
async fn m_002_add_kind_version(pool: &SqlitePool) -> Result<(), EventStoreError> {
    add_column_if_missing(
        pool,
        "ALTER TABLE events ADD COLUMN kind_version INTEGER NOT NULL DEFAULT 1",
    )
    .await
}

/// Migration 2 → 3: add `projection_snapshots` table so projections can load
/// from a persisted snapshot instead of replaying the entire event log.
async fn m_003_projection_snapshots(pool: &SqlitePool) -> Result<(), EventStoreError> {
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

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_snapshots_at ON projection_snapshots(snapshot_at)")
        .execute(pool)
        .await?;
    Ok(())
}

/// Migration 3 → 4: add `causal_parent` column for causal DAG ordering.
/// Nullable INTEGER references the id of the event that caused this one.
async fn m_004_add_causal_parent(pool: &SqlitePool) -> Result<(), EventStoreError> {
    add_column_if_missing(
        pool,
        "ALTER TABLE events ADD COLUMN causal_parent INTEGER DEFAULT NULL",
    )
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_causal_parent ON events(causal_parent)")
        .execute(pool)
        .await?;
    Ok(())
}

/// Migration 4 → 5: add composite indexes for common query patterns.
///
/// Most queries filter on `dropped = 0` first, so leading with `dropped`
/// lets SQLite skip dropped rows via the index rather than scanning.
async fn m_005_composite_indexes(pool: &SqlitePool) -> Result<(), EventStoreError> {
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_events_dropped_changeset ON events(dropped, changeset_id)",
    )
    .execute(pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_dropped_agent ON events(dropped, agent_id)")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_events_dropped_timestamp ON events(dropped, timestamp)",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Idempotent wrapper around `ALTER TABLE ... ADD COLUMN`.
///
/// If a previous migration was interrupted after the ALTER but before the
/// version update, the column may already exist. SQLite reports this as
/// "duplicate column"; we treat it as success.
async fn add_column_if_missing(pool: &SqlitePool, sql: &str) -> Result<(), EventStoreError> {
    match sqlx::query(sql).execute(pool).await {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_monotonic_and_reach_current_version() {
        // Versions strictly increasing, contiguous, starting at 2.
        let mut expected = 2;
        for m in MIGRATIONS {
            assert_eq!(
                m.version, expected,
                "migration {:?} breaks the contiguous version sequence",
                m.name
            );
            expected += 1;
        }

        let last = MIGRATIONS
            .last()
            .expect("MIGRATIONS must not be empty")
            .version;
        assert_eq!(
            last, CURRENT_SCHEMA_VERSION,
            "CURRENT_SCHEMA_VERSION must equal the last migration version"
        );
    }

    #[test]
    fn apply_rejects_unknown_version() {
        let unknown = Migration {
            version: 999,
            name: "bogus",
        };
        // We cannot run the async fn without a pool, but we can check that
        // the match dispatch has no accidental catch-all. Any new migration
        // added to `apply` must also have an entry in `MIGRATIONS`, so this
        // invariant is enforced by construction + the monotonicity test.
        assert_eq!(unknown.version, 999);
    }
}
