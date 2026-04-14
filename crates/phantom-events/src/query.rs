//! Advanced query capabilities for the event store.
//!
//! [`EventQuery`] provides a flexible filter builder with optional fields
//! for agent, changeset, symbol, timestamp, and result limit.

use chrono::{DateTime, Utc};
use phantom_core::id::{AgentId, ChangesetId, SymbolId};

/// Result ordering for [`EventQuery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOrder {
    /// Oldest first (chronological).
    Asc,
    /// Newest first.
    Desc,
}

impl Default for QueryOrder {
    fn default() -> Self {
        Self::Desc
    }
}

impl QueryOrder {
    /// SQL keyword for this ordering direction.
    pub(crate) fn as_sql(&self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

/// A flexible query filter for events.
///
/// Set any combination of fields to narrow results. `None` fields are ignored.
/// Results are ordered by [`QueryOrder::Desc`] (newest first) by default.
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
    /// Filter by event kind — matches events whose JSON `kind` column starts
    /// with any of these prefixes (e.g. `"ChangesetSubmitted"`,
    /// `"ChangesetMaterialized"`). Empty means no kind filter.
    pub kind_prefixes: Vec<String>,
    /// Result ordering. Defaults to [`QueryOrder::Desc`] (newest first).
    pub order: QueryOrder,
}
