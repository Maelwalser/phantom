//! SQLite-backed append-only event store.
//!
//! [`SqliteEventStore`] implements [`phantom_core::EventStore`] using SQLite
//! in WAL mode via sqlx, providing an async connection pool that avoids
//! thread starvation in async contexts.

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::Row;
use tracing::debug;

use phantom_core::error::CoreError;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::error::EventStoreError;

/// Current schema version for the event store database.
///
/// Increment this when adding migrations in [`SqliteEventStore::run_migrations`].
const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Configuration for [`SqliteEventStore`].
pub struct EventStoreConfig {
    /// Maximum number of connections in the SQLite pool.
    ///
    /// SQLite WAL mode allows concurrent readers with a single writer.
    /// Higher values help read-heavy workloads (multiple agents querying
    /// status concurrently).
    pub max_connections: u32,
}

impl Default for EventStoreConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
        }
    }
}

/// Parse a single SQLite row into an [`Event`].
///
/// Expects columns: `id`, `timestamp`, `changeset_id`, `agent_id`, `kind`.
/// Unrecognized `EventKind` variants — whether unit variants (caught by
/// `#[serde(other)]`) or data-carrying variants from newer schema versions
/// (caught by the fallback match) — are returned as [`EventKind::Unknown`]
/// instead of propagating an error, ensuring forward compatibility.
pub(crate) fn row_to_event(row: &SqliteRow) -> Result<Event, EventStoreError> {
    let id: i64 = row.get("id");
    let ts_str: String = row.get("timestamp");
    let changeset_id: String = row.get("changeset_id");
    let agent_id: String = row.get("agent_id");
    let kind_json: String = row.get("kind");

    let timestamp = DateTime::parse_from_rfc3339(&ts_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| EventStoreError::InvalidTimestamp(ts_str, e.to_string()))?;
    let kind: EventKind = match serde_json::from_str(&kind_json) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(
                event_id = id,
                kind_json,
                error = %e,
                "unrecognized EventKind, falling back to Unknown"
            );
            EventKind::Unknown
        }
    };

    Ok(Event {
        id: EventId(id as u64),
        timestamp,
        changeset_id: ChangesetId(changeset_id),
        agent_id: AgentId(agent_id),
        kind,
    })
}

/// An append-only event store backed by a SQLite database in WAL mode.
///
/// Uses sqlx's async connection pool, eliminating the need for a blocking
/// `Mutex<Connection>` that would stall async executor threads.
pub struct SqliteEventStore {
    pub(crate) pool: SqlitePool,
}

impl SqliteEventStore {
    /// Open or create an event store at the given file path.
    ///
    /// Enables WAL mode, sets a 5-second busy timeout, enables foreign keys,
    /// and runs schema migrations. Uses [`EventStoreConfig::default`] for pool
    /// settings.
    pub async fn open(path: &Path) -> Result<Self, EventStoreError> {
        Self::open_with_config(path, EventStoreConfig::default()).await
    }

    /// Open or create an event store with explicit configuration.
    pub async fn open_with_config(
        path: &Path,
        config: EventStoreConfig,
    ) -> Result<Self, EventStoreError> {
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&url)
            .await?;
        let store = Self { pool };
        store.configure().await?;
        store.ensure_schema().await?;
        store.run_migrations().await?;
        debug!(?path, "opened event store");
        Ok(store)
    }

    /// Create an in-memory event store for testing.
    pub async fn in_memory() -> Result<Self, EventStoreError> {
        // In-memory databases are per-connection, so we use a single
        // connection to keep the database alive and consistent.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        let store = Self { pool };
        store.configure().await?;
        store.ensure_schema().await?;
        store.run_migrations().await?;
        debug!("opened in-memory event store");
        Ok(store)
    }

    /// Configure SQLite pragmas.
    async fn configure(&self) -> Result<(), EventStoreError> {
        sqlx::query("PRAGMA journal_mode = WAL")
            .execute(&self.pool)
            .await?;
        sqlx::query("PRAGMA busy_timeout = 5000")
            .execute(&self.pool)
            .await?;
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Create the events table, schema_meta table, and indexes if they do not exist.
    async fn ensure_schema(&self) -> Result<(), EventStoreError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        // Seed initial schema version if not present.
        sqlx::query(
            "INSERT OR IGNORE INTO schema_meta (key, value) VALUES ('schema_version', '1')",
        )
        .execute(&self.pool)
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
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_changeset ON events(changeset_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_agent ON events(agent_id)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp)")
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Read the current schema version from the database.
    async fn schema_version(&self) -> Result<u32, EventStoreError> {
        let row: (String,) =
            sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
                .fetch_one(&self.pool)
                .await?;
        row.0
            .parse()
            .map_err(|_| EventStoreError::SchemaMismatch {
                expected: CURRENT_SCHEMA_VERSION,
                found: 0,
            })
    }

    /// Run forward migrations up to [`CURRENT_SCHEMA_VERSION`].
    async fn run_migrations(&self) -> Result<(), EventStoreError> {
        let version = self.schema_version().await?;

        if version < 2 {
            // Migration 1 → 2: add kind_version column for envelope versioning.
            sqlx::query(
                "ALTER TABLE events ADD COLUMN kind_version INTEGER NOT NULL DEFAULT 1",
            )
            .execute(&self.pool)
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
                .execute(&self.pool)
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

    /// Append an event, returning the auto-generated [`EventId`].
    async fn append_internal(&self, event: Event) -> Result<EventId, EventStoreError> {
        let kind_json = serde_json::to_string(&event.kind)?;
        let timestamp_str = event.timestamp.to_rfc3339();

        let result = sqlx::query(
            "INSERT INTO events (timestamp, changeset_id, agent_id, kind, kind_version)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&timestamp_str)
        .bind(&event.changeset_id.0)
        .bind(&event.agent_id.0)
        .bind(&kind_json)
        .bind(1i32)
        .execute(&self.pool)
        .await?;

        let id = result.last_insert_rowid() as u64;
        Ok(EventId(id))
    }

    /// Read events from a query with the given WHERE clause and positional parameters.
    pub(crate) async fn query_events(
        &self,
        where_clause: &str,
        params: &[String],
    ) -> Result<Vec<Event>, EventStoreError> {
        let sql = format!(
            "SELECT id, timestamp, changeset_id, agent_id, kind
             FROM events
             WHERE {where_clause}
             ORDER BY id ASC"
        );

        let mut query = sqlx::query(&sql);
        for param in params {
            query = query.bind(param);
        }

        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_event).collect()
    }
}

#[async_trait::async_trait]
impl EventStore for SqliteEventStore {
    async fn append(&self, event: Event) -> Result<EventId, CoreError> {
        self.append_internal(event).await.map_err(Into::into)
    }

    async fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError> {
        self.query_events("changeset_id = $1 AND dropped = 0", std::slice::from_ref(&id.0))
            .await
            .map_err(Into::into)
    }

    async fn query_by_agent(&self, id: &AgentId) -> Result<Vec<Event>, CoreError> {
        self.query_events("agent_id = $1 AND dropped = 0", std::slice::from_ref(&id.0))
            .await
            .map_err(Into::into)
    }

    async fn query_all(&self) -> Result<Vec<Event>, CoreError> {
        self.query_events("dropped = 0", &[])
            .await
            .map_err(Into::into)
    }

    async fn query_since(&self, since: DateTime<Utc>) -> Result<Vec<Event>, CoreError> {
        let ts = since.to_rfc3339();
        self.query_events("timestamp >= $1 AND dropped = 0", &[ts])
            .await
            .map_err(Into::into)
    }
}
