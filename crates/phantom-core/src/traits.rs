//! Trait interfaces that downstream crates implement.
//!
//! These traits define the contract between `phantom-core` and the rest of
//! the workspace. Keeping the trait definitions here ensures that `phantom-core`
//! remains dependency-free of other Phantom crates while still dictating the
//! interfaces they must satisfy.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::changeset::SemanticOperation;
use crate::conflict::ConflictDetail;
use crate::error::CoreError;
use crate::event::Event;
use crate::id::{AgentId, ChangesetId, EventId};
use crate::symbol::SymbolEntry;

/// Append-only event store interface.
///
/// Implemented by `phantom-events` (backed by SQLite in WAL mode).
pub trait EventStore: Send + Sync {
    /// Append a new event and return its auto-assigned ID.
    fn append(&self, event: Event) -> Result<EventId, CoreError>;

    /// Return all events belonging to the given changeset.
    fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError>;

    /// Return all events produced by the given agent.
    fn query_by_agent(&self, id: &AgentId) -> Result<Vec<Event>, CoreError>;

    /// Return every event in insertion order.
    fn query_all(&self) -> Result<Vec<Event>, CoreError>;

    /// Return events whose timestamp is at or after `since`.
    fn query_since(&self, since: DateTime<Utc>) -> Result<Vec<Event>, CoreError>;
}

/// Live symbol index over the current trunk state.
///
/// Implemented by `phantom-semantic`. Updated after each materialization.
pub trait SymbolIndex: Send + Sync {
    /// Look up a single symbol by its ID.
    fn lookup(&self, id: &crate::id::SymbolId) -> Option<SymbolEntry>;

    /// Return all symbols defined in the given file.
    fn symbols_in_file(&self, path: &Path) -> Vec<SymbolEntry>;

    /// Return every symbol in the index.
    fn all_symbols(&self) -> Vec<SymbolEntry>;

    /// Replace the symbol set for a file.
    fn update_file(&mut self, path: &Path, symbols: Vec<SymbolEntry>);

    /// Remove all symbols associated with a file.
    fn remove_file(&mut self, path: &Path);
}

/// Semantic analysis: symbol extraction, diffing, and three-way merge.
///
/// Implemented by `phantom-semantic` using tree-sitter grammars.
pub trait SemanticAnalyzer: Send + Sync {
    /// Parse a file and extract its symbols.
    fn extract_symbols(
        &self,
        path: &Path,
        content: &[u8],
    ) -> Result<Vec<SymbolEntry>, CoreError>;

    /// Compute the semantic operations needed to transform `base` into `current`.
    fn diff_symbols(
        &self,
        base: &[SymbolEntry],
        current: &[SymbolEntry],
    ) -> Vec<SemanticOperation>;

    /// Perform a three-way semantic merge.
    fn three_way_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeResult, CoreError>;
}

/// Outcome of a three-way semantic merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeResult {
    /// The merge produced clean output.
    Clean(Vec<u8>),
    /// The merge found conflicts that require re-dispatch.
    Conflict(Vec<ConflictDetail>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_merge_result_roundtrip() {
        let clean = MergeResult::Clean(b"merged output".to_vec());
        let json = serde_json::to_string(&clean).unwrap();
        let back: MergeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(clean, back);

        let conflict = MergeResult::Conflict(vec![]);
        let json = serde_json::to_string(&conflict).unwrap();
        let back: MergeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(conflict, back);
    }
}
