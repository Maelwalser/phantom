//! SQLite-backed append-only event store.
//!
//! [`SqliteEventStore`] implements [`phantom_core::EventStore`] using SQLite
//! in WAL mode via sqlx, providing an async connection pool that avoids
//! thread starvation in async contexts.

use std::path::Path;

use chrono::{DateTime, Utc};
use std::str::FromStr;

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use tracing::debug;

use phantom_core::error::CoreError;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::error::EventStoreError;
use crate::schema;

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
            tracing::debug!(
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

/// Tracks SQL WHERE conditions and their bound parameter values.
///
/// Eliminates manual `$N` placeholder counting when building dynamic queries.
struct QueryBuilder {
    conditions: Vec<String>,
    params: Vec<String>,
}

impl QueryBuilder {
    fn new() -> Self {
        Self {
            conditions: vec!["dropped = 0".into()],
            params: Vec::new(),
        }
    }

    /// Register a parameter value and return its positional placeholder (e.g. `$3`).
    fn bind(&mut self, value: String) -> String {
        self.params.push(value);
        format!("${}", self.params.len())
    }

    /// Add a WHERE condition.
    fn push(&mut self, condition: String) {
        self.conditions.push(condition);
    }

    fn where_clause(&self) -> String {
        self.conditions.join(" AND ")
    }

    /// Execute the query against `pool`, returning parsed events.
    async fn fetch(
        &self,
        pool: &SqlitePool,
        order: &str,
        limit: Option<u64>,
    ) -> Result<Vec<Event>, EventStoreError> {
        let where_clause = self.where_clause();
        let limit_clause = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
        let sql = format!(
            "SELECT id, timestamp, changeset_id, agent_id, kind
             FROM events
             WHERE {where_clause}
             ORDER BY id {order}{limit_clause}"
        );

        let mut query = sqlx::query(&sql);
        for param in &self.params {
            query = query.bind(param);
        }

        let rows = query.fetch_all(pool).await?;
        rows.iter().map(row_to_event).collect()
    }
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
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .pragma("journal_mode", "WAL")
            .pragma("busy_timeout", "5000")
            .pragma("foreign_keys", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(config.max_connections)
            .connect_with(options)
            .await?;
        let store = Self { pool };
        schema::ensure_schema(&store.pool).await?;
        schema::run_migrations(&store.pool).await?;
        debug!(?path, "opened event store");
        Ok(store)
    }

    /// Create an in-memory event store for testing.
    pub async fn in_memory() -> Result<Self, EventStoreError> {
        // In-memory databases are per-connection, so we use a single
        // connection to keep the database alive and consistent.
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .pragma("journal_mode", "WAL")
            .pragma("busy_timeout", "5000")
            .pragma("foreign_keys", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let store = Self { pool };
        schema::ensure_schema(&store.pool).await?;
        schema::run_migrations(&store.pool).await?;
        debug!("opened in-memory event store");
        Ok(store)
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

    /// Execute a flexible query against the event store.
    ///
    /// Results are ordered by `id DESC` (newest first) by default, suitable
    /// for "show me the N most recent events" use cases like `phantom log`.
    pub async fn query(&self, q: &crate::query::EventQuery) -> Result<Vec<Event>, EventStoreError> {
        let mut qb = QueryBuilder::new();

        if let Some(ref agent) = q.agent_id {
            let p = qb.bind(agent.0.clone());
            qb.push(format!("agent_id = {p}"));
        }

        if let Some(ref cs) = q.changeset_id {
            let p = qb.bind(cs.0.clone());
            qb.push(format!("changeset_id = {p}"));
        }

        if let Some(ref sym) = q.symbol_id {
            let p = qb.bind(sym.0.clone());
            qb.push(format!("kind LIKE '%' || {p} || '%'"));
        }

        if let Some(ref since) = q.since {
            let p = qb.bind(since.to_rfc3339());
            qb.push(format!("timestamp >= {p}"));
        }

        if !q.kind_prefixes.is_empty() {
            let or_parts: Vec<String> = q
                .kind_prefixes
                .iter()
                .map(|prefix| {
                    let p = qb.bind(format!("{{\"{prefix}\""));
                    format!("kind LIKE {p} || '%'")
                })
                .collect();
            qb.push(format!("({})", or_parts.join(" OR ")));
        }

        qb.fetch(&self.pool, q.order.as_sql(), q.limit).await
    }

    /// Mark all events belonging to a changeset as dropped.
    ///
    /// Returns the number of rows affected.
    pub async fn mark_dropped(&self, changeset_id: &ChangesetId) -> Result<u64, EventStoreError> {
        let result = sqlx::query("UPDATE events SET dropped = 1 WHERE changeset_id = $1")
            .bind(&changeset_id.0)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Read events matching a simple WHERE clause with positional parameters.
    ///
    /// Results are always ordered by `id ASC` (chronological), as expected
    /// by the [`EventStore`] trait methods.
    pub(crate) async fn query_events(
        &self,
        where_clause: &str,
        params: &[String],
    ) -> Result<Vec<Event>, EventStoreError> {
        let mut qb = QueryBuilder::new();
        // Replace the default "dropped = 0" with the caller's full clause
        // which already includes the dropped filter.
        qb.conditions = vec![where_clause.to_string()];
        for p in params {
            qb.params.push(p.clone());
        }
        qb.fetch(&self.pool, "ASC", None).await
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
}
