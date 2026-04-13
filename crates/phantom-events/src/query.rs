//! Advanced query capabilities for the event store.
//!
//! [`EventQuery`] provides a flexible filter builder with optional fields
//! for agent, changeset, symbol, timestamp, and result limit.

use chrono::{DateTime, Utc};
use phantom_core::id::{AgentId, ChangesetId, SymbolId};

use crate::error::EventStoreError;
use crate::store::{row_to_event, SqliteEventStore};

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
    pub async fn query(
        &self,
        q: &EventQuery,
    ) -> Result<Vec<phantom_core::Event>, EventStoreError> {
        let mut conditions = vec!["dropped = 0".to_string()];
        let mut param_values: Vec<String> = Vec::new();

        if let Some(ref agent) = q.agent_id {
            param_values.push(agent.0.clone());
            conditions.push(format!("agent_id = ${}", param_values.len()));
        }

        if let Some(ref cs) = q.changeset_id {
            param_values.push(cs.0.clone());
            conditions.push(format!("changeset_id = ${}", param_values.len()));
        }

        if let Some(ref sym) = q.symbol_id {
            param_values.push(sym.0.clone());
            conditions.push(format!("kind LIKE '%' || ${} || '%'", param_values.len()));
        }

        if let Some(ref since) = q.since {
            param_values.push(since.to_rfc3339());
            conditions.push(format!("timestamp >= ${}", param_values.len()));
        }

        let where_clause = conditions.join(" AND ");
        let limit_clause = q.limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();

        let sql = format!(
            "SELECT id, timestamp, changeset_id, agent_id, kind
             FROM events
             WHERE {where_clause}
             ORDER BY id DESC{limit_clause}"
        );

        let mut query = sqlx::query(&sql);
        for param in &param_values {
            query = query.bind(param);
        }

        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_event).collect()
    }

    /// Mark all events belonging to a changeset as dropped.
    ///
    /// Returns the number of rows affected.
    pub async fn mark_dropped(&self, changeset_id: &ChangesetId) -> Result<u64, EventStoreError> {
        let result =
            sqlx::query("UPDATE events SET dropped = 1 WHERE changeset_id = $1")
                .bind(&changeset_id.0)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected())
    }
}
