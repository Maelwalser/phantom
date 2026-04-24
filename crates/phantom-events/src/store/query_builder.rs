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

/// Sort order for event queries built by [`QueryBuilder`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// Tracks SQL WHERE conditions and their bound parameter values.
///
/// `conditions` and `params` are intentionally private: exposing raw
/// `WHERE` fragments to callers would defeat the parameterization that
/// prevents SQL injection. Use [`push`](Self::push) and [`bind`](Self::bind).
pub(super) struct QueryBuilder {
    conditions: Vec<String>,
    params: Vec<String>,
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

    /// Add a WHERE condition. The fragment must be caller-constructed from
    /// fixed SQL tokens and placeholders returned by [`bind`](Self::bind);
    /// never embed user input directly.
    pub(super) fn push(&mut self, condition: String) {
        self.conditions.push(condition);
    }

    /// Replace all existing conditions. Used by the crate-internal
    /// `query_events` escape hatch; the caller-supplied clause must
    /// contain only fixed SQL and `$N` placeholders.
    pub(super) fn replace_conditions(&mut self, conditions: Vec<String>) {
        self.conditions = conditions;
    }

    fn where_clause(&self) -> String {
        self.conditions.join(" AND ")
    }

    /// Execute the query against `pool`, returning parsed events.
    pub(super) async fn fetch(
        &self,
        pool: &SqlitePool,
        order: SortDir,
        limit: Option<u64>,
    ) -> Result<Vec<Event>, EventStoreError> {
        let where_clause = self.where_clause();
        let limit_clause = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
        let order_sql = order.as_sql();
        let sql = format!(
            "SELECT id, timestamp, changeset_id, agent_id, kind, causal_parent
             FROM events
             WHERE {where_clause}
             ORDER BY id {order_sql}{limit_clause}"
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
        // Escape SQL LIKE metacharacters (%, _, \) before binding so that a
        // symbol id containing those characters matches literally rather than
        // acting as a wildcard.
        let p = qb.bind(escape_like(&sym.0));
        qb.push(format!("kind LIKE '%' || {p} || '%' ESCAPE '\\'"));
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
                // like_prefix produces a `{"KindName"` fragment; the trailing
                // `%` is the wildcard we want, but any `%`/`_`/`\` inside the
                // caller-supplied prefix must be escaped so they cannot act
                // as wildcards themselves.
                let p = qb.bind(escape_like(&kind_pattern::like_prefix(prefix)));
                format!("kind LIKE {p} || '%' ESCAPE '\\'")
            })
            .collect();
        qb.push(format!("({})", or_parts.join(" OR ")));
    }
}

/// Escape SQL LIKE metacharacters (`%`, `_`, `\`) so the value matches
/// literally under an `ESCAPE '\'` clause.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
