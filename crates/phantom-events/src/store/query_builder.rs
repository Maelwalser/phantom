//! Dynamic WHERE-clause builder with positional parameter binding.
//!
//! [`QueryBuilder`] eliminates manual `$N` placeholder counting when
//! assembling event queries with variable combinations of filters.
//! [`apply_event_filters`] translates an [`EventQuery`](crate::query::EventQuery)
//! into WHERE conditions and is shared between `query()` and `count()` so
//! their filter semantics cannot drift apart.

use phantom_core::event::Event;
use sqlx::sqlite::SqlitePool;

use crate::error::EventStoreError;
use crate::kind_pattern;
use crate::query::EventQuery;

use super::row::row_to_event;

/// Tracks SQL WHERE conditions and their bound parameter values.
pub(super) struct QueryBuilder {
    pub(super) conditions: Vec<String>,
    pub(super) params: Vec<String>,
}

impl QueryBuilder {
    pub(super) fn new() -> Self {
        Self {
            conditions: vec!["dropped = 0".into()],
            params: Vec::new(),
        }
    }

    /// Register a parameter value and return its positional placeholder (e.g. `$3`).
    pub(super) fn bind(&mut self, value: String) -> String {
        self.params.push(value);
        format!("${}", self.params.len())
    }

    /// Add a WHERE condition.
    pub(super) fn push(&mut self, condition: String) {
        self.conditions.push(condition);
    }

    fn where_clause(&self) -> String {
        self.conditions.join(" AND ")
    }

    /// Execute the query against `pool`, returning parsed events.
    pub(super) async fn fetch(
        &self,
        pool: &SqlitePool,
        order: &str,
        limit: Option<u64>,
    ) -> Result<Vec<Event>, EventStoreError> {
        let where_clause = self.where_clause();
        let limit_clause = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
        let sql = format!(
            "SELECT id, timestamp, changeset_id, agent_id, kind, causal_parent
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

    /// Execute a COUNT(*) query with the same filters (ignoring limit/order).
    pub(super) async fn fetch_count(&self, pool: &SqlitePool) -> Result<u64, EventStoreError> {
        let where_clause = self.where_clause();
        let sql = format!("SELECT COUNT(*) as cnt FROM events WHERE {where_clause}");

        let mut query = sqlx::query_scalar::<_, i64>(&sql);
        for param in &self.params {
            query = query.bind(param);
        }

        let count = query.fetch_one(pool).await?;
        Ok(count as u64)
    }
}

/// Translate an [`EventQuery`] into WHERE conditions on a [`QueryBuilder`].
///
/// Shared between [`SqliteEventStore::query`](super::SqliteEventStore::query)
/// and [`SqliteEventStore::count`](super::SqliteEventStore::count) so filter
/// semantics stay in lockstep.
pub(super) fn apply_event_filters(qb: &mut QueryBuilder, q: &EventQuery) {
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
                let p = qb.bind(kind_pattern::like_prefix(prefix));
                format!("kind LIKE {p} || '%'")
            })
            .collect();
        qb.push(format!("({})", or_parts.join(" OR ")));
    }
}
