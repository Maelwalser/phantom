//! Advanced query capabilities for the event store.
//!
//! [`EventQuery`] provides a flexible filter builder with optional fields
//! for agent, changeset, symbol, timestamp, and result limit.

use chrono::{DateTime, Utc};
use rusqlite::params_from_iter;
use serde::de::Error as _;

use phantom_core::id::{AgentId, ChangesetId, SymbolId};

use crate::error::EventStoreError;
use crate::store::SqliteEventStore;

/// A flexible query filter for events.
///
/// Set any combination of fields to narrow results. `None` fields are ignored.
#[derive(Debug, Default, Clone)]
pub struct EventQuery {
    /// Filter by agent.
    pub agent_id: Option<AgentId>,
    /// Filter by changeset.
    pub changeset_id: Option<ChangesetId>,
    /// Filter by symbol (searches the JSON `kind` column).
    pub symbol_id: Option<SymbolId>,
    /// Only events at or after this timestamp.
    pub since: Option<DateTime<Utc>>,
    /// Maximum number of events to return.
    pub limit: Option<u64>,
}

impl SqliteEventStore {
    /// Execute a flexible query against the event store.
    pub fn query(
        &self,
        q: &EventQuery,
    ) -> Result<Vec<phantom_core::Event>, EventStoreError> {
        let mut conditions = vec!["dropped = 0".to_string()];
        let mut param_values: Vec<String> = Vec::new();

        if let Some(ref agent) = q.agent_id {
            param_values.push(agent.0.clone());
            conditions.push(format!("agent_id = ?{}", param_values.len()));
        }

        if let Some(ref cs) = q.changeset_id {
            param_values.push(cs.0.clone());
            conditions.push(format!("changeset_id = ?{}", param_values.len()));
        }

        if let Some(ref sym) = q.symbol_id {
            param_values.push(sym.0.clone());
            conditions.push(format!("kind LIKE '%' || ?{} || '%'", param_values.len()));
        }

        if let Some(ref since) = q.since {
            param_values.push(since.to_rfc3339());
            conditions.push(format!("timestamp >= ?{}", param_values.len()));
        }

        let where_clause = conditions.join(" AND ");
        let limit_clause = q
            .limit
            .map(|n| format!(" LIMIT {n}"))
            .unwrap_or_default();

        let sql = format!(
            "SELECT id, timestamp, changeset_id, agent_id, kind
             FROM events
             WHERE {where_clause}
             ORDER BY id ASC{limit_clause}"
        );

        let conn = self.conn.lock().expect("lock poisoned");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(param_values.iter()), |row| {
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
            let kind: phantom_core::EventKind = serde_json::from_str(&kind_json)?;
            events.push(phantom_core::Event {
                id: phantom_core::id::EventId(id),
                timestamp,
                changeset_id: phantom_core::id::ChangesetId(changeset_id),
                agent_id: phantom_core::id::AgentId(agent_id),
                kind,
            });
        }
        Ok(events)
    }

    /// Mark all events belonging to a changeset as dropped.
    ///
    /// Returns the number of rows affected.
    pub fn mark_dropped(
        &self,
        changeset_id: &ChangesetId,
    ) -> Result<u64, EventStoreError> {
        let conn = self.conn.lock().expect("lock poisoned");
        let affected = conn.execute(
            "UPDATE events SET dropped = 1 WHERE changeset_id = ?1",
            [&changeset_id.0],
        )?;
        Ok(affected as u64)
    }
}
