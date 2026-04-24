//! Schema creation and migration logic for the event store database.
//!
//! Separated from [`crate::store`] to isolate DDL and version tracking from
//! connection management and query execution.
//!
//! - [`ddl`] creates the v1 baseline (tables and single-column indexes).
//! - [`migrations`] carries the registry of forward migrations from v1 up
//!   to [`CURRENT_SCHEMA_VERSION`].

use sqlx::sqlite::SqlitePool;

use crate::error::EventStoreError;

mod ddl;
mod migrations;

pub(crate) use ddl::ensure_schema;
pub(crate) use migrations::CURRENT_SCHEMA_VERSION;

/// Read the current schema version from the database.
async fn schema_version(pool: &SqlitePool) -> Result<u32, EventStoreError> {
    let row: (String,) =
        sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
            .fetch_one(pool)
            .await?;
    row.0.parse().map_err(|_| EventStoreError::SchemaCorrupted {
        key: "schema_version".into(),
        value: row.0.clone(),
    })
}

/// Run forward migrations up to [`CURRENT_SCHEMA_VERSION`].
///
/// Each migration (DDL + version bump) runs inside a SQLite transaction so a
/// process death between the two statements cannot leave the database in a
/// state where the DDL has partly applied but the recorded version is stale.
/// The existing `IF NOT EXISTS` / `add_column_if_missing` guards remain as
/// defense in depth for migrations that are naturally idempotent.
pub(crate) async fn run_migrations(pool: &SqlitePool) -> Result<(), EventStoreError> {
    let current = schema_version(pool).await?;
    if current > CURRENT_SCHEMA_VERSION {
        return Err(EventStoreError::SchemaMismatch {
            expected: CURRENT_SCHEMA_VERSION,
            found: current,
        });
    }

    for m in migrations::MIGRATIONS
        .iter()
        .filter(|m| m.version > current)
    {
        tracing::debug!(target: "phantom_events::schema", to = m.version, name = m.name, "applying migration");
        let mut tx = pool.begin().await?;
        migrations::apply(m, &mut tx).await?;
        sqlx::query("UPDATE schema_meta SET value = $1 WHERE key = 'schema_version'")
            .bind(m.version.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }

    Ok(())
}
