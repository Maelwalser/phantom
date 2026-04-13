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
    /// A task was created (agent overlay provisioned).
    #[serde(alias = "OverlayCreated")]
    TaskCreated {
        /// The trunk commit the overlay is based on.
        base_commit: GitOid,
        /// Task description for the agent (empty for interactive sessions).
        #[serde(default)]
        task: String,
    },
    /// A task was destroyed (agent overlay torn down).
    #[serde(alias = "OverlayDestroyed")]
    TaskDestroyed,
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
    /// A background agent process was launched.
    AgentLaunched {
        /// PID of the background process.
        pid: u32,
        /// The task the agent is working on.
        task: String,
    },
    /// A background agent process completed.
    AgentCompleted {
        /// Exit code of the process (None if killed by signal).
        exit_code: Option<i32>,
        /// Whether auto-materialize succeeded.
        materialized: bool,
    },
    /// A live rebase was performed on an agent's overlay after trunk advanced.
    LiveRebased {
        /// The agent's base commit before the rebase.
        old_base: GitOid,
        /// The new trunk commit the agent is now based on.
        new_base: GitOid,
        /// Files that were cleanly merged into the agent's upper layer.
        merged_files: Vec<PathBuf>,
        /// Files that had conflicts and were left unchanged in the upper layer.
        conflicted_files: Vec<PathBuf>,
    },
    /// Unrecognized event kind from a newer schema version.
    ///
    /// Preserved in the event log but skipped during replay and projection.
    /// This ensures that older Phantom binaries can still read databases
    /// written by newer versions without crashing.
    #[serde(other)]
    Unknown,
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
            kind: EventKind::TaskCreated {
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
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }]);
        let json = serde_json::to_string(&conflicted).unwrap();
        let back: MergeCheckResult = serde_json::from_str(&json).unwrap();
        assert_eq!(conflicted, back);
    }

    #[test]
    fn serde_all_event_kinds() {
        let kinds = vec![
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: String::new(),
            },
            EventKind::TaskDestroyed,
            EventKind::FileWritten {
                path: PathBuf::from("src/main.rs"),
                content_hash: ContentHash::from_bytes(b"test"),
            },
            EventKind::FileDeleted {
                path: PathBuf::from("old.rs"),
            },
            EventKind::ChangesetSubmitted { operations: vec![] },
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
            EventKind::LiveRebased {
                old_base: GitOid::zero(),
                new_base: GitOid::from_bytes([2; 20]),
                merged_files: vec![PathBuf::from("src/merged.rs")],
                conflicted_files: vec![PathBuf::from("src/conflict.rs")],
            },
            EventKind::AgentLaunched {
                pid: 12345,
                task: "add rate limiting".into(),
            },
            EventKind::AgentCompleted {
                exit_code: Some(0),
                materialized: true,
            },
        ];

        for kind in &kinds {
            let json = serde_json::to_string(kind).unwrap();
            let back: EventKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back, "round-trip failed for {kind:?}");
        }
    }

    #[test]
    fn unrecognized_variant_deserializes_as_unknown() {
        // Simulate a future EventKind variant that this binary doesn't know about.
        let json = r#""SomeFutureVariant""#;
        let kind: EventKind = serde_json::from_str(json).unwrap();
        assert_eq!(kind, EventKind::Unknown);
    }

    #[test]
    fn unrecognized_variant_with_data_returns_error() {
        // serde(other) only catches unit variants. Object-shaped unknown
        // variants produce a deserialization error rather than silently
        // losing data. This is acceptable — the store's row_to_event
        // will surface this as an EventStoreError::Serialization.
        let json = r#"{"NewFeatureEvent":{"field":"value"}}"#;
        let result = serde_json::from_str::<EventKind>(json);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_variant_roundtrips_as_unknown() {
        let kind = EventKind::Unknown;
        let json = serde_json::to_string(&kind).unwrap();
        let back: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, EventKind::Unknown);
    }
}
