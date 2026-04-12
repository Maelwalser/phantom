//! Shared test helpers for integration tests.
//!
//! Provides [`TestContext`] — a self-contained test harness that creates a
//! temporary git repository, event store, and semantic merger for exercising
//! the full Phantom stack without FUSE.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use chrono::Utc;
use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_events::SqliteEventStore;
use phantom_orchestrator::git::GitOps;
use phantom_orchestrator::materializer::Materializer;
use phantom_semantic::SemanticMerger;
use tempfile::TempDir;

/// Self-contained test environment with a git repo, event store, and merger.
pub struct TestContext {
    pub dir: TempDir,
    pub git: GitOps,
    pub events: SqliteEventStore,
    pub merger: SemanticMerger,
}

impl TestContext {
    /// Create a test git repo with an initial empty commit, plus an in-memory
    /// event store and semantic merger.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("failed to create temp dir");
        let repo = git2::Repository::init(dir.path()).expect("failed to init repo");

        // Create initial commit so HEAD exists.
        let mut index = repo.index().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@phantom").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
            .unwrap();

        let git = GitOps::open(dir.path()).expect("failed to open repo");
        let events = SqliteEventStore::in_memory().expect("failed to create event store");
        let merger = SemanticMerger::new();

        Self {
            dir,
            git,
            events,
            merger,
        }
    }

    /// Commit files to the repository and return the new HEAD OID.
    ///
    /// `files` is a slice of `(relative_path, content)` pairs.
    pub fn commit_files(&self, files: &[(&str, &str)]) -> GitOid {
        let trunk_path = self.git.repo().workdir().unwrap().to_path_buf();

        for &(path, content) in files {
            let full = trunk_path.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, content).unwrap();
        }

        let mut index = self.git.repo().index().unwrap();
        for &(path, _) in files {
            index.add_path(Path::new(path)).unwrap();
        }
        index.write().unwrap();

        let tree_oid = index.write_tree().unwrap();
        let tree = self.git.repo().find_tree(tree_oid).unwrap();
        let sig = git2::Signature::now("test", "test@phantom").unwrap();

        let head = self.git.repo().head().unwrap();
        let parent_oid = head.target().unwrap();
        let parent = self.git.repo().find_commit(parent_oid).unwrap();

        let new_oid = self
            .git
            .repo()
            .commit(Some("HEAD"), &sig, &sig, "test commit", &tree, &[&parent])
            .unwrap();

        phantom_orchestrator::git::oid_to_git_oid(new_oid)
    }

    /// Create a temporary upper directory for an agent with the given files.
    ///
    /// Returns `(AgentId, upper_dir TempDir)`.
    pub fn create_agent(&self, agent_name: &str, files: &[(&str, &str)]) -> (AgentId, TempDir) {
        let upper = TempDir::new().expect("failed to create upper dir");
        for &(path, content) in files {
            let full = upper.path().join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full, content).unwrap();
        }
        (AgentId(agent_name.into()), upper)
    }

    /// Build a [`Changeset`] for a given agent.
    pub fn build_changeset(
        &self,
        cs_id: &str,
        agent_id: &AgentId,
        base_commit: GitOid,
        files_touched: Vec<PathBuf>,
        task: &str,
    ) -> Changeset {
        Changeset {
            id: ChangesetId(cs_id.into()),
            agent_id: agent_id.clone(),
            task: task.into(),
            base_commit,
            files_touched,
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

    /// Create a [`Materializer`] backed by this context's git repo.
    ///
    /// This re-opens the repository because `Materializer` takes ownership of
    /// a `GitOps`.
    pub fn materializer(&self) -> Materializer {
        let git = GitOps::open(self.dir.path()).expect("failed to reopen repo");
        Materializer::new(git)
    }

    /// Return the current HEAD OID.
    pub fn head(&self) -> GitOid {
        self.git.head_oid().expect("failed to get HEAD")
    }

    /// Read a file from the current trunk working tree.
    pub fn read_trunk_file(&self, path: &str) -> String {
        let trunk_path = self.git.repo().workdir().unwrap().join(path);
        std::fs::read_to_string(&trunk_path)
            .unwrap_or_else(|e| panic!("failed to read trunk file {path}: {e}"))
    }

    /// Read a file from the git object store at the current HEAD.
    pub fn read_file_at_head(&self, path: &str) -> String {
        let head = self.head();
        let content = self
            .git
            .read_file_at_commit(&head, Path::new(path))
            .unwrap_or_else(|e| panic!("failed to read {path} at HEAD: {e}"));
        String::from_utf8(content).expect("file is not valid UTF-8")
    }
}
