//! Changeset types — the atomic unit of work in Phantom.
//!
//! A [`Changeset`] captures everything an agent produced for a single task:
//! which files were touched, what semantic operations were performed, and the
//! current lifecycle status. Changesets replace the traditional branch model
//! and are designed to be reorderable when their symbol sets are disjoint.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, ChangesetId, ContentHash, GitOid, SymbolId};
use crate::symbol::SymbolEntry;

/// Lifecycle status of a changeset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangesetStatus {
    /// Agent is still working inside its overlay.
    InProgress,
    /// Agent finished; changeset is awaiting merge check.
    Submitted,
    /// Semantic merge is in progress.
    Merging,
    /// Successfully committed to trunk.
    Materialized,
    /// Semantic conflict detected; needs re-task.
    Conflicted,
    /// A conflict resolution agent is actively working on this changeset.
    Resolving,
    /// Rolled back / removed via event log replay.
    Dropped,
}

/// A structured description of a single change an agent made.
///
/// Operations are expressed in terms of symbols rather than raw text lines,
/// enabling Phantom to reason about conflicts at the semantic level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SemanticOperation {
    /// A new symbol was added to a file.
    AddSymbol {
        /// File the symbol was added to.
        file: PathBuf,
        /// The new symbol entry.
        symbol: SymbolEntry,
    },
    /// An existing symbol was modified.
    ModifySymbol {
        /// File containing the symbol.
        file: PathBuf,
        /// Content hash before the modification.
        old_hash: ContentHash,
        /// The updated symbol entry.
        new_entry: SymbolEntry,
    },
    /// A symbol was removed from a file.
    DeleteSymbol {
        /// File the symbol was removed from.
        file: PathBuf,
        /// Identity of the deleted symbol.
        id: SymbolId,
    },
    /// A new file was created.
    AddFile {
        /// Path of the new file.
        path: PathBuf,
    },
    /// A file was deleted.
    DeleteFile {
        /// Path of the removed file.
        path: PathBuf,
    },
    /// A change the semantic layer could not classify.
    ///
    /// Falls back to a raw text patch for line-based merging.
    RawDiff {
        /// File that was changed.
        path: PathBuf,
        /// Unified diff patch.
        patch: String,
    },
}

impl SemanticOperation {
    /// Return the file path this operation applies to.
    pub fn file_path(&self) -> &std::path::Path {
        match self {
            Self::AddSymbol { file, .. }
            | Self::ModifySymbol { file, .. }
            | Self::DeleteSymbol { file, .. } => file,
            Self::AddFile { path } | Self::DeleteFile { path } | Self::RawDiff { path, .. } => path,
        }
    }

    /// Return the symbol name this operation affects, if it's a symbol-level
    /// operation.
    ///
    /// Returns `None` for `AddFile`, `DeleteFile`, and `RawDiff` which don't
    /// operate on a specific symbol.
    pub fn symbol_name(&self) -> Option<&str> {
        match self {
            Self::AddSymbol { symbol, .. } => Some(&symbol.name),
            Self::ModifySymbol { new_entry, .. } => Some(&new_entry.name),
            Self::DeleteSymbol { id, .. } => Some(&id.0),
            Self::AddFile { .. } | Self::DeleteFile { .. } | Self::RawDiff { .. } => None,
        }
    }
}

/// Aggregated test results for a changeset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    /// Number of passing tests.
    pub passed: u32,
    /// Number of failing tests.
    pub failed: u32,
    /// Number of skipped tests.
    pub skipped: u32,
}

/// The atomic unit of work in Phantom.
///
/// When an agent is assigned a task it produces a changeset — not a branch.
/// Changesets whose symbol sets are disjoint can be materialized in any order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changeset {
    /// Unique identifier (e.g. `"cs-0042"`).
    pub id: ChangesetId,
    /// Which agent produced this changeset.
    pub agent_id: AgentId,
    /// Human-readable task description.
    pub task: String,
    /// The trunk commit this changeset was built against.
    pub base_commit: GitOid,
    /// Files touched (quick overlap detection before semantic analysis).
    pub files_touched: Vec<PathBuf>,
    /// Semantic operations extracted after the agent finishes.
    pub operations: Vec<SemanticOperation>,
    /// Test results if the agent ran the suite.
    pub test_result: Option<TestResult>,
    /// When this changeset was created.
    pub created_at: DateTime<Utc>,
    /// Current lifecycle status.
    pub status: ChangesetStatus,
    /// PID of the background agent process, if launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_pid: Option<u32>,
    /// When the background agent was launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_launched_at: Option<DateTime<Utc>>,
    /// When the background agent completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_completed_at: Option<DateTime<Utc>>,
    /// Exit code of the background agent process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_exit_code: Option<i32>,
}

#[cfg(test)]
#[path = "changeset_tests.rs"]
mod tests;
