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
    row.0.parse().map_err(|_| EventStoreError::SchemaMismatch {
        expected: CURRENT_SCHEMA_VERSION,
        found: 0,
    })
}

/// Run forward migrations up to [`CURRENT_SCHEMA_VERSION`].
pub(crate) async fn run_migrations(pool: &SqlitePool) -> Result<(), EventStoreError> {
    let current = schema_version(pool).await?;
    if current > CURRENT_SCHEMA_VERSION {
        return Err(EventStoreError::SchemaMismatch {
            expected: CURRENT_SCHEMA_VERSION,
            found: current,
        });
    }

    for m in migrations::MIGRATIONS.iter().filter(|m| m.version > current) {
        tracing::debug!(target: "phantom_events::schema", to = m.version, name = m.name, "applying migration");
        migrations::apply(m, pool).await?;
        sqlx::query("UPDATE schema_meta SET value = $1 WHERE key = 'schema_version'")
            .bind(m.version.to_string())
            .execute(pool)
            .await?;
    }

    Ok(())
}
