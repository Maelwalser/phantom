//! Connection pool construction and schema-marker file management.
//!
//! Centralizes the SQLite pragma block used by both the file-backed and
//! in-memory constructors, so the two code paths cannot drift apart.

use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

use crate::error::EventStoreError;
use crate::schema;

use super::config::EventStoreConfig;

/// Source of SQLite connection options — a file path or an in-memory URL.
pub(super) enum PoolSource<'a> {
    File(&'a Path),
    InMemory,
}

/// Build a pool with the standard pragma configuration shared by every
/// `SqliteEventStore` entry point.
pub(super) async fn build_pool(
    source: PoolSource<'_>,
    config: &EventStoreConfig,
) -> Result<SqlitePool, EventStoreError> {
    let options = match source {
        PoolSource::File(path) => SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
        PoolSource::InMemory => SqliteConnectOptions::from_str("sqlite::memory:")?,
    }
    .pragma("journal_mode", "WAL")
    .pragma("busy_timeout", "5000")
    .pragma("foreign_keys", "ON")
    .pragma("synchronous", "NORMAL")
    .pragma("cache_size", "-64000")
    .pragma("temp_store", "MEMORY");

    let pool = SqlitePoolOptions::new()
        .max_connections(config.max_connections)
        .connect_with(options)
        .await?;
    Ok(pool)
}

/// Write a version marker file next to `path` so subsequent opens can
/// skip the schema/migration check. Sweeps stale markers from older
/// schema versions. All operations are best-effort: a failure here is
/// not fatal since [`schema::ensure_schema`] and [`schema::run_migrations`]
/// are idempotent.
pub(super) fn maintain_schema_marker(path: &Path) {
    let marker = path.with_extension(format!("schema_v{}", schema::CURRENT_SCHEMA_VERSION));
    // Write marker atomically (tmp + rename) so a crash never leaves
    // a half-written file that tricks future opens.
    let tmp = marker.with_extension("tmp");
    let _ = std::fs::write(&tmp, schema::CURRENT_SCHEMA_VERSION.to_string());
    let _ = std::fs::rename(&tmp, &marker);
    // Clean up markers from older schema versions.
    for v in 1..schema::CURRENT_SCHEMA_VERSION {
        let old = path.with_extension(format!("schema_v{v}"));
        let _ = std::fs::remove_file(old);
    }
}

/// Returns true if a marker file for the current schema version already
/// exists next to `path`. Used to skip schema work on every open.
pub(super) fn marker_exists(path: &Path) -> bool {
    path.with_extension(format!("schema_v{}", schema::CURRENT_SCHEMA_VERSION))
        .exists()
}
