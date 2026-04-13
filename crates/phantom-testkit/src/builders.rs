//! Builder helpers for constructing test fixtures.

#![allow(dead_code)]

use std::path::PathBuf;

use chrono::Utc;
use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::id::{AgentId, ChangesetId, GitOid};

/// Build a minimal [`Changeset`] for unit/integration tests.
pub fn make_changeset(id: &str, base: GitOid, files: Vec<PathBuf>) -> Changeset {
    Changeset {
        id: ChangesetId(id.into()),
        agent_id: AgentId("agent-test".into()),
        task: "test task".into(),
        base_commit: base,
        files_touched: files,
        operations: vec![],
        test_result: None,
        created_at: Utc::now(),
        status: ChangesetStatus::Submitted,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
}
