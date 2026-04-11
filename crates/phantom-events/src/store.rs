//! SQLite-backed append-only event store.
//!
//! [`SqliteEventStore`] implements [`phantom_core::EventStore`] using SQLite
//! in WAL mode, providing concurrent readers with a single writer.

use std::path::Path;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::de::Error as _;
use tracing::debug;

use phantom_core::error::CoreError;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::error::EventStoreError;

/// An append-only event store backed by a SQLite database in WAL mode.
///
/// The inner connection is wrapped in a [`Mutex`] so that `SqliteEventStore`
/// is `Send + Sync` as required by [`EventStore`].
pub struct SqliteEventStore {
    pub(crate) conn: Mutex<Connection>,
}

impl SqliteEventStore {
    /// Open or create an event store at the given file path.
    ///
    /// Enables WAL mode, sets a 5-second busy timeout, enables foreign keys,
    /// and runs schema migrations.
    pub fn open(path: &Path) -> Result<Self, EventStoreError> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.configure()?;
        store.ensure_schema()?;
        debug!(?path, "opened event store");
        Ok(store)
    }

    /// Create an in-memory event store for testing.
    pub fn in_memory() -> Result<Self, EventStoreError> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.configure()?;
        store.ensure_schema()?;
        debug!("opened in-memory event store");
        Ok(store)
    }

    /// Configure SQLite pragmas.
    fn configure(&self) -> Result<(), EventStoreError> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;
        Ok(())
    }

    /// Create the events table and indexes if they do not exist.
    fn ensure_schema(&self) -> Result<(), EventStoreError> {
        let conn = self.conn.lock().expect("lock poisoned");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp    TEXT NOT NULL,
                changeset_id TEXT NOT NULL,
                agent_id     TEXT NOT NULL,
                kind         TEXT NOT NULL,
                dropped      INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_events_changeset ON events(changeset_id);
            CREATE INDEX IF NOT EXISTS idx_events_agent ON events(agent_id);
            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);",
        )?;
        Ok(())
    }

    /// Append an event, returning the auto-generated [`EventId`].
    fn append_internal(&self, event: Event) -> Result<EventId, EventStoreError> {
        let kind_json = serde_json::to_string(&event.kind)?;
        let timestamp_str = event.timestamp.to_rfc3339();
        let conn = self.conn.lock().expect("lock poisoned");

        conn.execute(
            "INSERT INTO events (timestamp, changeset_id, agent_id, kind)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                timestamp_str,
                event.changeset_id.0,
                event.agent_id.0,
                kind_json,
            ],
        )?;

        let id = conn.last_insert_rowid() as u64;
        Ok(EventId(id))
    }

    /// Read events from a query with the given WHERE clause and parameters.
    pub(crate) fn query_events(
        &self,
        where_clause: &str,
        params: &[&dyn rusqlite::types::ToSql],
    ) -> Result<Vec<Event>, EventStoreError> {
        let sql = format!(
            "SELECT id, timestamp, changeset_id, agent_id, kind
             FROM events
             WHERE {where_clause}
             ORDER BY id ASC"
        );
        let conn = self.conn.lock().expect("lock poisoned");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params, |row| {
            let id: u64 = row.get::<_, i64>(0)? as u64;
            let ts_str: String = row.get(1)?;
            let changeset_id: String = row.get(2)?;
            let agent_id: String = row.get(3)?;
            let kind_json: String = row.get(4)?;
            Ok((id, ts_str, changeset_id, agent_id, kind_json))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (id, ts_str, changeset_id, agent_id, kind_json) = row?;
            let timestamp = DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    EventStoreError::Serialization(serde_json::Error::custom(format!(
                        "invalid timestamp: {e}"
                    )))
                })?;
            let kind: EventKind = serde_json::from_str(&kind_json)?;
            events.push(Event {
                id: EventId(id),
                timestamp,
                changeset_id: ChangesetId(changeset_id),
                agent_id: AgentId(agent_id),
                kind,
            });
        }
        Ok(events)
    }
}

impl EventStore for SqliteEventStore {
    fn append(&self, event: Event) -> Result<EventId, CoreError> {
        self.append_internal(event).map_err(Into::into)
    }

    fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError> {
        self.query_events("changeset_id = ?1 AND dropped = 0", &[&id.0])
            .map_err(Into::into)
    }

    fn query_by_agent(&self, id: &AgentId) -> Result<Vec<Event>, CoreError> {
        self.query_events("agent_id = ?1 AND dropped = 0", &[&id.0])
            .map_err(Into::into)
    }

    fn query_all(&self) -> Result<Vec<Event>, CoreError> {
        self.query_events("dropped = 0", &[]).map_err(Into::into)
    }

    fn query_since(&self, since: DateTime<Utc>) -> Result<Vec<Event>, CoreError> {
        let ts = since.to_rfc3339();
        self.query_events("timestamp >= ?1 AND dropped = 0", &[&ts])
            .map_err(Into::into)
    }
}
