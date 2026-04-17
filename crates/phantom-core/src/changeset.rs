//! Changeset types — the atomic unit of work in Phantom.
//!
//! A [`Changeset`] captures everything an agent produced for a single task:
//! which files were touched, what semantic operations were performed, and the
//! current lifecycle status. Changesets replace the traditional branch model
//! and are designed to be reorderable when their symbol sets are disjoint.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::id::{AgentId, ChangesetId, ContentHash, GitOid, SymbolId};
use crate::symbol::SymbolEntry;

/// Lifecycle status of a changeset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangesetStatus {
    /// Agent is still working inside its overlay.
    InProgress,
    /// Changeset submitted and merged to trunk successfully.
    #[serde(alias = "Materialized", alias = "Merging")]
    Submitted,
    /// Semantic conflict detected; needs re-task.
    Conflicted,
    /// A conflict resolution agent is actively working on this changeset.
    Resolving,
    /// Rolled back / removed via event log replay.
    Dropped,
}

impl std::fmt::Display for ChangesetStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InProgress => write!(f, "in progress"),
            Self::Submitted => write!(f, "submitted"),
            Self::Conflicted => write!(f, "conflicted"),
            Self::Resolving => write!(f, "resolving"),
            Self::Dropped => write!(f, "dropped"),
        }
    }
}

impl ChangesetStatus {
    /// Return `true` if transitioning from `self` to `next` is legal.
    ///
    /// The transition table:
    /// * `InProgress` → `Submitted`, `Conflicted`, `Dropped`
    /// * `Conflicted` → `Resolving`, `Dropped`
    /// * `Resolving`  → `Submitted`, `Conflicted`, `Dropped`
    /// * `Submitted`  → `Dropped` (only via rollback)
    /// * `Dropped`    → terminal, no outgoing transitions
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        use ChangesetStatus::{Conflicted, Dropped, InProgress, Resolving, Submitted};
        matches!(
            (self, next),
            (InProgress | Resolving, Submitted | Conflicted | Dropped)
                | (Conflicted, Resolving | Dropped)
                | (Submitted, Dropped)
        )
    }

    /// Validate a transition and return `next` on success, or a
    /// [`CoreError::InvalidStatusTransition`] describing the rejected pair.
    pub fn try_transition_to(self, next: Self) -> Result<Self, CoreError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(CoreError::InvalidStatusTransition {
                from: self,
                to: next,
            })
        }
    }
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

impl Changeset {
    /// Construct a fresh [`Changeset`] for a newly-created agent overlay.
    ///
    /// The returned changeset has:
    /// * `status = ChangesetStatus::InProgress`
    /// * empty `files_touched` and `operations`
    /// * `test_result = None`
    /// * all four `agent_*` process fields = `None`
    /// * `created_at = Utc::now()`
    ///
    /// Centralizing this default-shell construction means that new fields
    /// added to [`Changeset`] only need to be handled in one place rather
    /// than at every struct-literal call site in downstream crates.
    pub fn new_shell(
        id: ChangesetId,
        agent_id: AgentId,
        task: impl Into<String>,
        base_commit: GitOid,
    ) -> Self {
        Self {
            id,
            agent_id,
            task: task.into(),
            base_commit,
            files_touched: Vec::new(),
            operations: Vec::new(),
            test_result: None,
            created_at: Utc::now(),
            status: ChangesetStatus::InProgress,
            agent_pid: None,
            agent_launched_at: None,
            agent_completed_at: None,
            agent_exit_code: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SymbolId;
    use crate::symbol::SymbolKind;

    fn sample_changeset() -> Changeset {
        Changeset {
            id: ChangesetId("cs-0001".into()),
            agent_id: AgentId("agent-a".into()),
            task: "add rate limiting".into(),
            base_commit: GitOid::zero(),
            files_touched: vec![PathBuf::from("src/api.rs")],
            operations: vec![SemanticOperation::AddSymbol {
                file: PathBuf::from("src/api.rs"),
                symbol: SymbolEntry {
                    id: SymbolId("crate::api::rate_limit::Function".into()),
                    kind: SymbolKind::Function,
                    name: "rate_limit".into(),
                    scope: "crate::api".into(),
                    file: PathBuf::from("src/api.rs"),
                    byte_range: 0..50,
                    content_hash: ContentHash::from_bytes(b"fn rate_limit() {}"),
                },
            }],
            test_result: Some(TestResult {
                passed: 10,
                failed: 0,
                skipped: 1,
            }),
            created_at: Utc::now(),
            status: ChangesetStatus::Submitted,
            agent_pid: None,
            agent_launched_at: None,
            agent_completed_at: None,
            agent_exit_code: None,
        }
    }

    #[test]
    fn serde_changeset_status_roundtrip() {
        for status in [
            ChangesetStatus::InProgress,
            ChangesetStatus::Submitted,
            ChangesetStatus::Conflicted,
            ChangesetStatus::Resolving,
            ChangesetStatus::Dropped,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: ChangesetStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn serde_semantic_operation_roundtrip() {
        let ops = vec![
            SemanticOperation::AddFile {
                path: PathBuf::from("new.rs"),
            },
            SemanticOperation::DeleteFile {
                path: PathBuf::from("old.rs"),
            },
            SemanticOperation::RawDiff {
                path: PathBuf::from("config.toml"),
                patch: "+foo = true".into(),
            },
        ];
        for op in &ops {
            let json = serde_json::to_string(op).unwrap();
            let back: SemanticOperation = serde_json::from_str(&json).unwrap();
            assert_eq!(*op, back);
        }
    }

    #[test]
    fn serde_changeset_roundtrip() {
        let cs = sample_changeset();
        let json = serde_json::to_string(&cs).unwrap();
        let back: Changeset = serde_json::from_str(&json).unwrap();
        assert_eq!(cs, back);
    }

    #[test]
    fn serde_test_result_roundtrip() {
        let tr = TestResult {
            passed: 5,
            failed: 2,
            skipped: 0,
        };
        let json = serde_json::to_string(&tr).unwrap();
        let back: TestResult = serde_json::from_str(&json).unwrap();
        assert_eq!(tr, back);
    }

    #[test]
    fn new_shell_populates_defaults() {
        let cs = Changeset::new_shell(
            ChangesetId("cs-shell".into()),
            AgentId("agent-shell".into()),
            "seed task",
            GitOid::zero(),
        );

        assert_eq!(cs.id, ChangesetId("cs-shell".into()));
        assert_eq!(cs.agent_id, AgentId("agent-shell".into()));
        assert_eq!(cs.task, "seed task");
        assert_eq!(cs.base_commit, GitOid::zero());
        assert!(cs.files_touched.is_empty());
        assert!(cs.operations.is_empty());
        assert!(cs.test_result.is_none());
        assert_eq!(cs.status, ChangesetStatus::InProgress);
        assert!(cs.agent_pid.is_none());
        assert!(cs.agent_launched_at.is_none());
        assert!(cs.agent_completed_at.is_none());
        assert!(cs.agent_exit_code.is_none());
    }

    #[test]
    fn can_transition_to_full_table() {
        use ChangesetStatus::{Conflicted, Dropped, InProgress, Resolving, Submitted};

        // Permitted transitions.
        let allowed = [
            (InProgress, Submitted),
            (InProgress, Conflicted),
            (InProgress, Dropped),
            (Conflicted, Resolving),
            (Conflicted, Dropped),
            (Resolving, Submitted),
            (Resolving, Conflicted),
            (Resolving, Dropped),
            (Submitted, Dropped),
        ];
        for (from, to) in allowed {
            assert!(
                from.can_transition_to(to),
                "expected {from:?} → {to:?} to be allowed"
            );
        }

        // A representative set of forbidden transitions.
        let forbidden = [
            (InProgress, Resolving),
            (InProgress, InProgress),
            (Submitted, InProgress),
            (Submitted, Conflicted),
            (Submitted, Submitted),
            (Conflicted, InProgress),
            (Conflicted, Submitted),
            (Resolving, InProgress),
            (Resolving, Resolving),
            (Dropped, InProgress),
            (Dropped, Submitted),
            (Dropped, Conflicted),
            (Dropped, Resolving),
            (Dropped, Dropped),
        ];
        for (from, to) in forbidden {
            assert!(
                !from.can_transition_to(to),
                "expected {from:?} → {to:?} to be forbidden"
            );
        }
    }

    #[test]
    fn try_transition_to_returns_next_on_success() {
        let next = ChangesetStatus::InProgress
            .try_transition_to(ChangesetStatus::Submitted)
            .unwrap();
        assert_eq!(next, ChangesetStatus::Submitted);
    }

    #[test]
    fn try_transition_to_reports_invalid() {
        let err = ChangesetStatus::Dropped
            .try_transition_to(ChangesetStatus::InProgress)
            .unwrap_err();
        match err {
            CoreError::InvalidStatusTransition { from, to } => {
                assert_eq!(from, ChangesetStatus::Dropped);
                assert_eq!(to, ChangesetStatus::InProgress);
            }
            other => panic!("expected InvalidStatusTransition, got {other:?}"),
        }
    }
}
