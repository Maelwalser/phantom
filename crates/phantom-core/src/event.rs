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
use crate::id::{AgentId, ChangesetId, ContentHash, EventId, GitOid, PlanId, SymbolId};

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
    /// A conflict resolution agent was launched for this changeset.
    ConflictResolutionStarted {
        /// The conflicts being resolved.
        conflicts: Vec<ConflictDetail>,
        /// Trunk HEAD at resolution time — becomes the new base_commit so
        /// post-resolution materialization uses the correct merge base.
        #[serde(default)]
        new_base: Option<GitOid>,
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
    /// A plan was created and agents dispatched.
    PlanCreated {
        /// Unique plan identifier.
        plan_id: PlanId,
        /// The original user request.
        request: String,
        /// Number of domains in the plan.
        domain_count: u32,
        /// Agent IDs dispatched for each domain.
        agent_ids: Vec<AgentId>,
    },
    /// A plan completed (all agents finished).
    PlanCompleted {
        /// Unique plan identifier.
        plan_id: PlanId,
        /// Number of domains that succeeded.
        succeeded: u32,
        /// Number of domains that failed.
        failed: u32,
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
#[path = "event_tests.rs"]
mod tests;
