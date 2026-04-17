//! Assemble a [`Changeset`] from the submission context, operations, and
//! touched files.

use std::path::PathBuf;

use chrono::Utc;

use phantom_core::changeset::{Changeset, ChangesetStatus, SemanticOperation};
use phantom_core::id::{AgentId, ChangesetId, GitOid};

/// Build the changeset passed to the materializer.
pub(super) fn build_changeset(
    changeset_id: &ChangesetId,
    agent_id: &AgentId,
    task: String,
    base_commit: GitOid,
    modified: &[PathBuf],
    deleted: &[PathBuf],
    all_ops: Vec<SemanticOperation>,
) -> Changeset {
    let mut files_touched: Vec<PathBuf> = modified.to_vec();
    files_touched.extend(deleted.iter().cloned());

    Changeset {
        id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        task,
        base_commit,
        files_touched,
        operations: all_ops,
        test_result: None,
        created_at: Utc::now(),
        status: ChangesetStatus::Submitted,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
}
