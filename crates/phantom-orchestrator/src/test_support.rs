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

    async fn latest_event_for_changeset(
        &self,
        id: &ChangesetId,
    ) -> Result<Option<EventId>, CoreError> {
        let events = self.events.read().unwrap();
        Ok(events
            .iter()
            .rev()
            .find(|e| e.changeset_id == *id)
            .map(|e| e.id))
    }
}

// ---------------------------------------------------------------------------
// GitOps test-only extensions
// ---------------------------------------------------------------------------

impl GitOps {
    /// Copy overlay files into the working tree, stage, and commit.
    ///
    /// This is intentionally **not** used in production (see
    /// `Materializer::commit_from_oids` for the atomic, OID-based approach).
    /// Retained for test helpers that need a quick way to advance trunk without
    /// going through the full materializer pipeline.
    pub(crate) fn commit_overlay_changes(
        &self,
        upper_dir: &Path,
        trunk_path: &Path,
        message: &str,
        author: &str,
    ) -> Result<GitOid, crate::error::OrchestratorError> {
        use crate::error::OrchestratorError;
        use crate::git::helpers::collect_files_recursive;

        let files = collect_files_recursive(upper_dir)?;

        for rel_path in &files {
            let src = upper_dir.join(rel_path);
            let dst = trunk_path.join(rel_path);

            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dst)?;
        }

        let repo = self.repo();
        let mut index = repo.index()?;
        for rel_path in &files {
            index.add_path(rel_path)?;
        }
        index.write()?;

        let tree_oid = index.write_tree()?;
        let tree = repo.find_tree(tree_oid)?;

        let sig = git2::Signature::now(author, &format!("{author}@phantom"))?;

        let parent_commit = match repo.head() {
            Ok(head) => {
                let oid = head
                    .target()
                    .ok_or_else(|| OrchestratorError::NotFound("HEAD has no target".into()))?;
                Some(repo.find_commit(oid)?)
            }
            Err(e) if e.code() == git2::ErrorCode::UnbornBranch => None,
            Err(e) => return Err(OrchestratorError::Git(e)),
        };

        let parents: Vec<&git2::Commit<'_>> = parent_commit.iter().collect();
        let new_oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;

        Ok(crate::git::oid_to_git_oid(new_oid))
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
