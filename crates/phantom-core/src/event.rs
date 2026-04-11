//! Event types for Phantom's append-only event log.
//!
//! Every action in Phantom — overlay creation, file writes, changeset
//! submission, materialization, rollback — is recorded as an immutable
//! [`Event`]. The event log is the source of truth for auditability,
//! surgical rollback, and replay.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::changeset::{SemanticOperation, TestResult};
use crate::conflict::ConflictDetail;
use crate::id::{AgentId, ChangesetId, ContentHash, EventId, GitOid, SymbolId};

/// Result of a semantic merge check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeCheckResult {
    /// The changeset merges cleanly with trunk.
    Clean,
    /// The changeset has symbol-level conflicts.
    Conflicted(Vec<ConflictDetail>),
}

/// The payload of an event — what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// An agent overlay was created.
    OverlayCreated {
        /// The trunk commit the overlay is based on.
        base_commit: GitOid,
        /// Task description for the agent (empty for interactive sessions).
        #[serde(default)]
        task: String,
    },
    /// An agent overlay was destroyed.
    OverlayDestroyed,
    /// An agent wrote a file inside its overlay.
    FileWritten {
        /// Path relative to the repo root.
        path: PathBuf,
        /// BLAKE3 hash of the written content.
        content_hash: ContentHash,
    },
    /// An agent deleted a file inside its overlay.
    FileDeleted {
        /// Path relative to the repo root.
        path: PathBuf,
    },
    /// An agent submitted its changeset for merge.
    ChangesetSubmitted {
        /// Semantic operations extracted from the changeset.
        operations: Vec<SemanticOperation>,
    },
    /// A merge check was performed on the changeset.
    ChangesetMergeChecked {
        /// Whether the merge was clean or conflicted.
        result: MergeCheckResult,
    },
    /// The changeset was materialized (committed to trunk).
    ChangesetMaterialized {
        /// The new trunk commit OID.
        new_commit: GitOid,
    },
    /// The changeset had semantic conflicts.
    ChangesetConflicted {
        /// Details of each conflict.
        conflicts: Vec<ConflictDetail>,
    },
    /// The changeset was dropped (rolled back).
    ChangesetDropped {
        /// Reason the changeset was dropped.
        reason: String,
    },
    /// Trunk advanced to a new commit.
    TrunkAdvanced {
        /// Previous trunk commit.
        old_commit: GitOid,
        /// New trunk commit.
        new_commit: GitOid,
    },
    /// An agent was notified that trunk symbols changed under it.
    AgentNotified {
        /// The agent that was notified.
        agent_id: AgentId,
        /// Symbols that changed in the agent's working set.
        changed_symbols: Vec<SymbolId>,
    },
    /// Test results were recorded.
    TestsRun(TestResult),
    /// An interactive CLI session was started inside the overlay.
    InteractiveSessionStarted {
        /// The command that was launched (e.g. "claude").
        command: String,
        /// PID of the spawned process (for stale session detection).
        pid: u32,
    },
    /// An interactive CLI session ended.
    InteractiveSessionEnded {
        /// Process exit code (`None` if killed by signal).
        exit_code: Option<i32>,
        /// Duration of the session in seconds.
        duration_secs: u64,
    },
}

/// An immutable record of something that happened in Phantom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Auto-incrementing identifier.
    pub id: EventId,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// The changeset this event belongs to.
    pub changeset_id: ChangesetId,
    /// The agent that caused this event.
    pub agent_id: AgentId,
    /// What happened.
    pub kind: EventKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> Event {
        Event {
            id: EventId(1),
            timestamp: Utc::now(),
            changeset_id: ChangesetId("cs-0001".into()),
            agent_id: AgentId("agent-a".into()),
            kind: EventKind::OverlayCreated {
                base_commit: GitOid::zero(),
                task: String::new(),
            },
        }
    }

    #[test]
    fn serde_event_roundtrip() {
        let event = sample_event();
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn serde_merge_check_result_roundtrip() {
        let clean = MergeCheckResult::Clean;
        let json = serde_json::to_string(&clean).unwrap();
        let back: MergeCheckResult = serde_json::from_str(&json).unwrap();
        assert_eq!(clean, back);

        let conflicted = MergeCheckResult::Conflicted(vec![ConflictDetail {
            kind: crate::conflict::ConflictKind::BothModifiedSymbol,
            file: PathBuf::from("src/lib.rs"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "test conflict".into(),
        }]);
        let json = serde_json::to_string(&conflicted).unwrap();
        let back: MergeCheckResult = serde_json::from_str(&json).unwrap();
        assert_eq!(conflicted, back);
    }

    #[test]
    fn serde_all_event_kinds() {
        let kinds = vec![
            EventKind::OverlayCreated {
                base_commit: GitOid::zero(),
                task: String::new(),
            },
            EventKind::OverlayDestroyed,
            EventKind::FileWritten {
                path: PathBuf::from("src/main.rs"),
                content_hash: ContentHash::from_bytes(b"test"),
            },
            EventKind::FileDeleted {
                path: PathBuf::from("old.rs"),
            },
            EventKind::ChangesetSubmitted {
                operations: vec![],
            },
            EventKind::ChangesetMergeChecked {
                result: MergeCheckResult::Clean,
            },
            EventKind::ChangesetMaterialized {
                new_commit: GitOid::zero(),
            },
            EventKind::ChangesetConflicted { conflicts: vec![] },
            EventKind::ChangesetDropped {
                reason: "reverted".into(),
            },
            EventKind::TrunkAdvanced {
                old_commit: GitOid::zero(),
                new_commit: GitOid::from_bytes([1; 20]),
            },
            EventKind::AgentNotified {
                agent_id: AgentId("agent-b".into()),
                changed_symbols: vec![SymbolId("mod::foo::Function".into())],
            },
            EventKind::TestsRun(TestResult {
                passed: 5,
                failed: 0,
                skipped: 1,
            }),
            EventKind::InteractiveSessionStarted {
                command: "claude".into(),
                pid: 12345,
            },
            EventKind::InteractiveSessionEnded {
                exit_code: Some(0),
                duration_secs: 300,
            },
        ];

        for kind in &kinds {
            let json = serde_json::to_string(kind).unwrap();
            let back: EventKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back, "round-trip failed for {kind:?}");
        }
    }
}
