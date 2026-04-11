//! Derive current codebase state from events.
//!
//! [`Projection`] iterates an event stream and builds a map of changeset
//! states, enabling queries like "which agents are active?" and "which
//! changesets are pending materialization?".

use std::collections::HashMap;

use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, GitOid};

/// Projected state derived from an event stream.
pub struct Projection {
    changesets: HashMap<ChangesetId, Changeset>,
}

impl Projection {
    /// Build a projection by replaying events in order.
    ///
    /// Each event updates the corresponding changeset record:
    /// - `OverlayCreated` → new changeset with `InProgress` status
    /// - `ChangesetSubmitted` → status becomes `Submitted`, operations stored
    /// - `ChangesetMaterialized` → status becomes `Materialized`
    /// - `ChangesetConflicted` → status becomes `Conflicted`
    /// - `ChangesetDropped` → status becomes `Dropped`
    /// - `TestsRun` → test results updated
    #[must_use]
    pub fn from_events(events: &[Event]) -> Self {
        let mut changesets: HashMap<ChangesetId, Changeset> = HashMap::new();

        for event in events {
            let cs = changesets
                .entry(event.changeset_id.clone())
                .or_insert_with(|| new_changeset(&event.changeset_id, &event.agent_id));

            match &event.kind {
                EventKind::OverlayCreated { base_commit, task } => {
                    cs.status = ChangesetStatus::InProgress;
                    cs.base_commit = *base_commit;
                    cs.task = task.clone();
                    cs.created_at = event.timestamp;
                }
                EventKind::ChangesetSubmitted { operations } => {
                    cs.status = ChangesetStatus::Submitted;
                    cs.operations = operations.clone();
                    // Extract touched file paths from semantic operations so that
                    // the materializer knows which files to merge.
                    for op in operations {
                        let path = match op {
                            phantom_core::changeset::SemanticOperation::AddSymbol {
                                file, ..
                            }
                            | phantom_core::changeset::SemanticOperation::ModifySymbol {
                                file, ..
                            }
                            | phantom_core::changeset::SemanticOperation::DeleteSymbol {
                                file, ..
                            } => Some(file.clone()),
                            phantom_core::changeset::SemanticOperation::AddFile { path } => {
                                Some(path.clone())
                            }
                            phantom_core::changeset::SemanticOperation::DeleteFile { path } => {
                                Some(path.clone())
                            }
                            phantom_core::changeset::SemanticOperation::RawDiff { path, .. } => {
                                Some(path.clone())
                            }
                        };
                        if let Some(p) = path {
                            if !cs.files_touched.contains(&p) {
                                cs.files_touched.push(p);
                            }
                        }
                    }
                }
                EventKind::ChangesetMaterialized { .. } => {
                    cs.status = ChangesetStatus::Materialized;
                }
                EventKind::ChangesetConflicted { .. } => {
                    cs.status = ChangesetStatus::Conflicted;
                }
                EventKind::ChangesetDropped { .. } => {
                    cs.status = ChangesetStatus::Dropped;
                }
                EventKind::TestsRun(result) => {
                    cs.test_result = Some(*result);
                }
                EventKind::FileWritten { path, .. } => {
                    if !cs.files_touched.contains(path) {
                        cs.files_touched.push(path.clone());
                    }
                }
                EventKind::FileDeleted { path } => {
                    if !cs.files_touched.contains(path) {
                        cs.files_touched.push(path.clone());
                    }
                }
                EventKind::InteractiveSessionStarted { .. } => {
                    cs.interactive_session_active = true;
                }
                EventKind::InteractiveSessionEnded { .. } => {
                    cs.interactive_session_active = false;
                }
                // Other event kinds don't affect changeset state.
                _ => {}
            }
        }

        Self { changesets }
    }

    /// Look up a changeset by ID.
    #[must_use]
    pub fn changeset(&self, id: &ChangesetId) -> Option<&Changeset> {
        self.changesets.get(id)
    }

    /// Return agents that have at least one `InProgress` changeset.
    #[must_use]
    pub fn active_agents(&self) -> Vec<AgentId> {
        let mut agents: Vec<AgentId> = self
            .changesets
            .values()
            .filter(|cs| cs.status == ChangesetStatus::InProgress)
            .map(|cs| cs.agent_id.clone())
            .collect();
        agents.sort_by(|a, b| a.0.cmp(&b.0));
        agents.dedup();
        agents
    }

    /// Return changesets with `Submitted` status.
    #[must_use]
    pub fn pending_changesets(&self) -> Vec<&Changeset> {
        let mut pending: Vec<&Changeset> = self
            .changesets
            .values()
            .filter(|cs| cs.status == ChangesetStatus::Submitted)
            .collect();
        pending.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        pending
    }
}

/// Create a default changeset shell for projection bookkeeping.
fn new_changeset(id: &ChangesetId, agent_id: &AgentId) -> Changeset {
    Changeset {
        id: id.clone(),
        agent_id: agent_id.clone(),
        task: String::new(),
        base_commit: GitOid::zero(),
        files_touched: Vec::new(),
        operations: Vec::new(),
        test_result: None,
        created_at: chrono::Utc::now(),
        status: ChangesetStatus::InProgress,
        interactive_session_active: false,
    }
}
