//! Event-to-changeset state reducer.
//!
//! The `match` on [`EventKind`] here is the single place where the
//! projection interprets the event stream. Keeping every state transition
//! colocated makes it easy to audit how each event kind affects a
//! [`Changeset`]; a trait-based handler registry would scatter that logic
//! across the crate for no real gain.

use std::collections::HashMap;

use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, GitOid};

/// Apply a sequence of events to a changeset map, updating each changeset
/// as events are encountered. Shared implementation for both full replay
/// ([`super::Projection::from_events`]) and incremental replay
/// ([`super::Projection::from_snapshot`]).
pub(super) fn apply_events(changesets: &mut HashMap<ChangesetId, Changeset>, events: &[Event]) {
    for event in events {
        let cs = changesets
            .entry(event.changeset_id.clone())
            .or_insert_with(|| new_changeset(&event.changeset_id, &event.agent_id));

        match &event.kind {
            EventKind::TaskCreated { base_commit, task } => {
                cs.status = ChangesetStatus::InProgress;
                cs.base_commit = *base_commit;
                cs.task.clone_from(task);
                cs.created_at = event.timestamp;
            }
            EventKind::ChangesetSubmitted { operations } => {
                cs.status = ChangesetStatus::Submitted;
                cs.operations.clone_from(operations);
                for op in operations {
                    let p = op.file_path().to_path_buf();
                    if !cs.files_touched.contains(&p) {
                        cs.files_touched.push(p);
                    }
                }
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
            EventKind::FileWritten { path, .. } | EventKind::FileDeleted { path }
                if !cs.files_touched.contains(path) =>
            {
                cs.files_touched.push(path.clone());
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
