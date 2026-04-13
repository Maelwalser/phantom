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

/// Parse a single SQLite row into an [`Event`].
///
/// Expects columns: `id`, `timestamp`, `changeset_id`, `agent_id`, `kind`.
pub(crate) fn row_to_event(row: &SqliteRow) -> Result<Event, EventStoreError> {
    let id: i64 = row.get("id");
    let ts_str: String = row.get("timestamp");
    let changeset_id: String = row.get("changeset_id");
    let agent_id: String = row.get("agent_id");
    let kind_json: String = row.get("kind");

    let timestamp = DateTime::parse_from_rfc3339(&ts_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| EventStoreError::InvalidTimestamp(ts_str, e.to_string()))?;
    let kind: EventKind = serde_json::from_str(&kind_json)?;

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
    /// and runs schema migrations.
    pub async fn open(path: &Path) -> Result<Self, EventStoreError> {
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await?;
        let store = Self { pool };
        store.configure().await?;
        store.ensure_schema().await?;
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

    /// Create the events table and indexes if they do not exist.
    async fn ensure_schema(&self) -> Result<(), EventStoreError> {
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

    /// Append an event, returning the auto-generated [`EventId`].
    async fn append_internal(&self, event: Event) -> Result<EventId, EventStoreError> {
        let kind_json = serde_json::to_string(&event.kind)?;
        let timestamp_str = event.timestamp.to_rfc3339();

        let result = sqlx::query(
            "INSERT INTO events (timestamp, changeset_id, agent_id, kind)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&timestamp_str)
        .bind(&event.changeset_id.0)
        .bind(&event.agent_id.0)
        .bind(&kind_json)
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
