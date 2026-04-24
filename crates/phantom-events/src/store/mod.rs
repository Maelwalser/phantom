//! SQLite-backed append-only event store.
//!
//! [`SqliteEventStore`] implements [`phantom_core::EventStore`] using SQLite
//! in WAL mode via sqlx, providing an async connection pool that avoids
//! thread starvation in async contexts.

use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::sqlite::SqlitePool;
use tracing::debug;

use phantom_core::error::CoreError;
use phantom_core::event::Event;
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::error::EventStoreError;
use crate::schema;

mod config;
mod connection;
mod queries;
pub(crate) mod query_builder;
pub(crate) mod row;

pub use config::EventStoreConfig;

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
    /// Every connection in the pool is configured at handshake time with
    /// WAL mode, a 5-second busy timeout, and foreign key enforcement.
    /// Uses [`EventStoreConfig::default`] for pool settings.
    pub async fn open(path: &Path) -> Result<Self, EventStoreError> {
        Self::open_with_config(path, EventStoreConfig::default()).await
    }

    /// Open or create an event store with explicit configuration.
    pub async fn open_with_config(
        path: &Path,
        config: EventStoreConfig,
    ) -> Result<Self, EventStoreError> {
        let pool = connection::build_pool(connection::PoolSource::File(path), &config).await?;
        let store = Self { pool };

        // Fast path: skip schema checks if a version marker indicates we are
        // already at the current schema version.
        if !connection::marker_exists(path) {
            schema::ensure_schema(&store.pool).await?;
            schema::run_migrations(&store.pool).await?;
            connection::maintain_schema_marker(path);
        }

        debug!(?path, "opened event store");
        Ok(store)
    }

    /// Create an in-memory event store for testing.
    pub async fn in_memory() -> Result<Self, EventStoreError> {
        // In-memory databases are per-connection, so we use a single
        // connection to keep the database alive and consistent.
        let pool = connection::build_pool(
            connection::PoolSource::InMemory,
            &EventStoreConfig { max_connections: 1 },
        )
        .await?;
        let store = Self { pool };
        schema::ensure_schema(&store.pool).await?;
        schema::run_migrations(&store.pool).await?;
        debug!("opened in-memory event store");
        Ok(store)
    }
}

#[async_trait::async_trait]
impl EventStore for SqliteEventStore {
    async fn append(&self, event: Event) -> Result<EventId, CoreError> {
        self.append_internal(event).await.map_err(Into::into)
    }

    async fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError> {
        self.query_events(
            "changeset_id = $1 AND dropped = 0",
            std::slice::from_ref(&id.0),
        )
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

    async fn latest_event_for_changeset(
        &self,
        id: &ChangesetId,
    ) -> Result<Option<EventId>, CoreError> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT id FROM events WHERE changeset_id = $1 AND dropped = 0
             ORDER BY id DESC LIMIT 1",
        )
        .bind(&id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(EventStoreError::from)?;
        row.map(|(id,)| row::checked_id(id, "id").map(EventId))
            .transpose()
            .map_err(Into::into)
    }
}
