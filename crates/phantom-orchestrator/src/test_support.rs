//! Shared test infrastructure for the orchestrator crate.
//!
//! Provides mock implementations and helper functions used across multiple
//! test modules to avoid duplication.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::{DateTime, Utc};

use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::error::CoreError;
use phantom_core::event::Event;
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;

use crate::git::GitOps;

// ---------------------------------------------------------------------------
// Mock EventStore
// ---------------------------------------------------------------------------

pub(crate) struct MockEventStore {
    events: RwLock<Vec<Event>>,
}

impl MockEventStore {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.read().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl EventStore for MockEventStore {
    async fn append(&self, event: Event) -> Result<EventId, CoreError> {
        let mut events = self.events.write().unwrap();
        let id = EventId(events.len() as u64 + 1);
        events.push(Event { id, ..event });
        Ok(id)
    }

    async fn query_by_changeset(&self, _id: &ChangesetId) -> Result<Vec<Event>, CoreError> {
        Ok(vec![])
    }

    async fn query_by_agent(&self, _id: &AgentId) -> Result<Vec<Event>, CoreError> {
        Ok(vec![])
    }

    async fn query_all(&self) -> Result<Vec<Event>, CoreError> {
        Ok(self.events.read().unwrap().clone())
    }

    async fn query_since(&self, _since: DateTime<Utc>) -> Result<Vec<Event>, CoreError> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Test repo helpers
// ---------------------------------------------------------------------------

/// Create a temporary git repo with an initial commit containing `files`.
pub(crate) fn init_repo(files: &[(&str, &[u8])]) -> (tempfile::TempDir, GitOps) {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();

    let mut index = repo.index().unwrap();
    for &(path, content) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();

    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("test", "test@phantom").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
        .unwrap();

    let ops = GitOps::open(dir.path()).unwrap();
    (dir, ops)
}

/// Commit additional changes on trunk (simulating another agent's materialization).
pub(crate) fn advance_trunk(git: &GitOps, files: &[(&str, &[u8])]) -> GitOid {
    let trunk_path = git.repo().workdir().unwrap().to_path_buf();
    let upper = tempfile::TempDir::new().unwrap();
    for &(path, content) in files {
        let full = upper.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
    }
    git.commit_overlay_changes(upper.path(), &trunk_path, "trunk advance", "other-agent")
        .unwrap()
}

/// Create an upper directory with the given files.
pub(crate) fn make_upper(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    for &(path, content) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
    }
    dir
}

/// Create a test changeset with the given ID, base commit, and files.
pub(crate) fn make_changeset(id: &str, base: GitOid, files: Vec<PathBuf>) -> Changeset {
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

/// Commit a single file and return the new HEAD OID.
pub(crate) fn commit_file(git: &GitOps, path: &str, content: &[u8], message: &str) -> GitOid {
    let repo = git.repo();
    let workdir = repo.workdir().unwrap();

    let full_path = workdir.join(path);
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full_path, content).unwrap();

    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])
        .unwrap();

    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(oid.as_bytes());
    GitOid::from_bytes(bytes)
}
