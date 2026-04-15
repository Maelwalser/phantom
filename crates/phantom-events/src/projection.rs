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
    /// - `TaskCreated` → new changeset with `InProgress` status
    /// - `ChangesetSubmitted` → status becomes `Submitted`, operations stored
    /// - `ChangesetMaterialized` → status stays `Submitted` (merge succeeded)
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
                EventKind::TaskCreated { base_commit, task } => {
                    cs.status = ChangesetStatus::InProgress;
                    cs.base_commit = *base_commit;
                    cs.task = task.clone();
                    cs.created_at = event.timestamp;
                }
                EventKind::ChangesetSubmitted { operations } => {
                    cs.status = ChangesetStatus::Submitted;
                    cs.operations = operations.clone();
                    for op in operations {
                        let p = op.file_path().to_path_buf();
                        if !cs.files_touched.contains(&p) {
                            cs.files_touched.push(p);
                        }
                    }
                }
                EventKind::ChangesetMaterialized { .. } => {
                    // Status stays Submitted — materialization is an internal
                    // detail, not a separate user-facing state.
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
                EventKind::AgentLaunched { pid, .. } => {
                    cs.agent_pid = Some(*pid);
                    cs.agent_launched_at = Some(event.timestamp);
                }
                EventKind::AgentCompleted { exit_code, .. } => {
                    cs.agent_exit_code = *exit_code;
                    cs.agent_completed_at = Some(event.timestamp);
                }
                EventKind::ConflictResolutionStarted { new_base, .. } => {
                    cs.status = ChangesetStatus::Resolving;
                    if let Some(base) = new_base {
                        cs.base_commit = *base;
                    }
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

    /// Find the most recently submitted changeset for a given agent.
    ///
    /// Returns the changeset with `Submitted` status whose `created_at` is
    /// latest among all submitted changesets belonging to `agent_id`.
    /// Returns `None` if no submitted changeset exists for that agent.
    #[must_use]
    pub fn latest_submitted_changeset(&self, agent_id: &AgentId) -> Option<&Changeset> {
        self.changesets
            .values()
            .filter(|cs| cs.agent_id == *agent_id && cs.status == ChangesetStatus::Submitted)
            .max_by_key(|cs| cs.created_at)
    }

    /// Find the most recently conflicted changeset for a given agent.
    ///
    /// Returns the changeset with `Conflicted` status whose `created_at` is
    /// latest among all conflicted changesets belonging to `agent_id`.
    /// Returns `None` if no conflicted changeset exists for that agent.
    #[must_use]
    pub fn latest_conflicted_changeset(&self, agent_id: &AgentId) -> Option<&Changeset> {
        self.changesets
            .values()
            .filter(|cs| cs.agent_id == *agent_id && cs.status == ChangesetStatus::Conflicted)
            .max_by_key(|cs| cs.created_at)
    }

    /// Find a changeset that is actively being resolved for a given agent.
    ///
    /// Returns the changeset with `Resolving` status belonging to `agent_id`,
    /// or `None` if no resolution is in progress.
    #[must_use]
    pub fn latest_resolving_changeset(&self, agent_id: &AgentId) -> Option<&Changeset> {
        self.changesets
            .values()
            .filter(|cs| cs.agent_id == *agent_id && cs.status == ChangesetStatus::Resolving)
            .max_by_key(|cs| cs.created_at)
    }

    /// Return all changesets belonging to the given agent, newest first.
    #[must_use]
    pub fn changesets_for_agent(&self, agent_id: &AgentId) -> Vec<&Changeset> {
        let mut result: Vec<&Changeset> = self
            .changesets
            .values()
            .filter(|cs| cs.agent_id == *agent_id)
            .collect();
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        result
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

    /// Return changesets with `Conflicted` or `Resolving` status.
    #[must_use]
    pub fn conflicted_changesets(&self) -> Vec<&Changeset> {
        let mut result: Vec<&Changeset> = self
            .changesets
            .values()
            .filter(|cs| {
                cs.status == ChangesetStatus::Conflicted
                    || cs.status == ChangesetStatus::Resolving
            })
            .collect();
        result.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        result
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
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
}