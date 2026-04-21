//! Trait interfaces that downstream crates implement.
//!
//! These traits define the contract between `phantom-core` and the rest of
//! the workspace. Keeping the trait definitions here ensures that `phantom-core`
//! remains dependency-free of other Phantom crates while still dictating the
//! interfaces they must satisfy.

use std::ops::Range;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::changeset::SemanticOperation;
use crate::conflict::MergeReport;
use crate::error::CoreError;
use crate::event::Event;
use crate::id::{AgentId, ChangesetId, EventId, SymbolId};
use crate::symbol::{ReferenceKind, SymbolEntry, SymbolReference};

/// Append-only event store interface.
///
/// Implemented by `phantom-events` (backed by SQLite in WAL mode via sqlx).
#[async_trait::async_trait]
pub trait EventStore: Send + Sync {
    /// Append a new event and return its auto-assigned ID.
    async fn append(&self, event: Event) -> Result<EventId, CoreError>;

    /// Return all events belonging to the given changeset.
    async fn query_by_changeset(&self, id: &ChangesetId) -> Result<Vec<Event>, CoreError>;

    /// Return all events produced by the given agent.
    async fn query_by_agent(&self, id: &AgentId) -> Result<Vec<Event>, CoreError>;

    /// Return every event in insertion order.
    async fn query_all(&self) -> Result<Vec<Event>, CoreError>;

    /// Return events whose timestamp is at or after `since`.
    async fn query_since(&self, since: DateTime<Utc>) -> Result<Vec<Event>, CoreError>;

    /// Return the ID of the most recent non-dropped event for a changeset.
    ///
    /// Used to determine the `causal_parent` when emitting lifecycle events
    /// within a changeset (e.g., `ChangesetSubmitted` → parent is `TaskCreated`).
    async fn latest_event_for_changeset(
        &self,
        id: &ChangesetId,
    ) -> Result<Option<EventId>, CoreError>;
}

/// Live symbol index over the current trunk state.
///
/// Implemented by `phantom-semantic`. Updated after each materialization.
pub trait SymbolIndex: Send + Sync {
    /// Look up a single symbol by its ID.
    fn lookup(&self, id: &SymbolId) -> Option<SymbolEntry>;

    /// Return all symbols defined in the given file.
    fn symbols_in_file(&self, path: &Path) -> Vec<SymbolEntry>;

    /// Return every symbol in the index.
    fn all_symbols(&self) -> Vec<SymbolEntry>;

    /// Return every symbol in the index whose short name equals `name`.
    ///
    /// Used by the dependency graph resolver for the "name + scope heuristic"
    /// — look up candidates by name, then disambiguate by scope when possible.
    /// Default impl filters `all_symbols` for convenience; concrete
    /// implementations may provide a faster index-backed implementation.
    fn lookup_by_name(&self, name: &str) -> Vec<SymbolEntry> {
        self.all_symbols()
            .into_iter()
            .filter(|s| s.name == name)
            .collect()
    }

    /// Replace the symbol set for a file.
    fn update_file(&mut self, path: &Path, symbols: Vec<SymbolEntry>);

    /// Remove all symbols associated with a file.
    fn remove_file(&mut self, path: &Path);
}

/// A resolved dependency edge in the semantic graph.
///
/// Edges are produced by resolving each [`SymbolReference`] against a live
/// [`SymbolIndex`]. A single `SymbolReference` may produce multiple edges when
/// the target name is ambiguous (over-approximation: better a false positive
/// notification than a missed impact).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyEdge {
    /// The symbol that holds the reference.
    pub source: SymbolId,
    /// The symbol being referenced.
    pub target: SymbolId,
    /// How the source references the target (call / type-use / import / ...).
    pub kind: ReferenceKind,
    /// File where the reference appears.
    pub file: PathBuf,
    /// Byte range of the reference node.
    pub byte_range: Range<usize>,
}

/// Semantic dependency graph over the current trunk state.
///
/// Tracks which symbols reference which others. Implemented by
/// `phantom-semantic`. Updated after each materialization in lockstep with
/// [`SymbolIndex`] so that impact analysis can translate "symbol X changed on
/// trunk" into "agent Y's working-set symbol Z depends on X, here is the edge."
pub trait DependencyGraph: Send + Sync {
    /// Every symbol that references `target` (reverse edges).
    fn dependents_of(&self, target: &SymbolId) -> Vec<DependencyEdge>;

    /// Every symbol that `source` references (forward edges).
    fn dependencies_of(&self, source: &SymbolId) -> Vec<DependencyEdge>;

    /// Replace the reference set for a file.
    ///
    /// Resolves each [`SymbolReference`] to concrete [`SymbolId`] targets via
    /// `index`. References whose name is not present in the index are dropped
    /// silently (typical for references to external crates / stdlib).
    fn update_file(&mut self, path: &Path, refs: Vec<SymbolReference>, index: &dyn SymbolIndex);

    /// Remove every edge whose `file` equals `path`.
    fn remove_file(&mut self, path: &Path);

    /// Return the number of edges in the graph (primarily for diagnostics
    /// and tests).
    fn edge_count(&self) -> usize;
}

/// Semantic analysis: symbol extraction, diffing, and three-way merge.
///
/// Implemented by `phantom-semantic` using tree-sitter grammars.
pub trait SemanticAnalyzer: Send + Sync {
    /// Parse a file and extract its symbols.
    fn extract_symbols(&self, path: &Path, content: &[u8]) -> Result<Vec<SymbolEntry>, CoreError>;

    /// Parse a file and extract its outbound symbol references.
    ///
    /// `symbols` is the list of symbols previously extracted from the same
    /// file — used by extractors to attribute each reference to its enclosing
    /// symbol via [`find_enclosing_symbol`](crate::symbol::find_enclosing_symbol).
    ///
    /// Languages without reference-extraction support return `Ok(Vec::new())`.
    /// This keeps the dependency-graph pipeline total across all supported
    /// file types — unknown languages just contribute no edges.
    fn extract_references(
        &self,
        _path: &Path,
        _content: &[u8],
        _symbols: &[SymbolEntry],
    ) -> Result<Vec<SymbolReference>, CoreError> {
        Ok(Vec::new())
    }

    /// Compute the semantic operations needed to transform `base` into `current`.
    fn diff_symbols(&self, base: &[SymbolEntry], current: &[SymbolEntry])
    -> Vec<SemanticOperation>;

    /// Perform a three-way semantic merge.
    ///
    /// Returns a [`MergeReport`] wrapping the outcome with the strategy that
    /// produced it, so callers can surface text-fallback cases to users.
    fn three_way_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeReport, CoreError>;

    /// Check if the given file path has a supported language for semantic
    /// analysis. Returns `false` by default.
    fn supports_language(&self, _path: &Path) -> bool {
        false
    }
}
