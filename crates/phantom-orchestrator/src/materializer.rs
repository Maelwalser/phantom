//! Changeset materialization — applying a changeset to trunk atomically.
//!
//! The [`Materializer`] coordinates git operations, semantic analysis, and
//! event logging to commit an agent's changeset to the shared trunk. It
//! handles three scenarios:
//!
//! 1. **Direct apply** — trunk hasn't advanced since the agent started; changes
//!    are committed without merging.
//! 2. **Clean merge** — trunk advanced but all changed files merge cleanly at
//!    the semantic level.
//! 3. **Conflict** — one or more files have symbol-level conflicts; the
//!    changeset is rejected and a conflict event is recorded.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::debug;

use phantom_core::changeset::{Changeset, SemanticOperation};
use phantom_core::conflict::ConflictDetail;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{EventId, GitOid};
use phantom_core::traits::{EventStore, MergeResult, SemanticAnalyzer};

use crate::error::OrchestratorError;
use crate::git::{self, GitOps};

/// Result of a materialization attempt.
#[derive(Debug)]
pub enum MaterializeResult {
    /// The changeset was successfully committed to trunk.
    Success {
        /// The new trunk commit OID.
        new_commit: GitOid,
    },
    /// The changeset had conflicts and was not committed.
    Conflict {
        /// Details of each conflict found.
        details: Vec<ConflictDetail>,
    },
}

/// Coordinates changeset materialization to trunk.
pub struct Materializer {
    git: GitOps,
}

impl Materializer {
    /// Create a materializer backed by the given git operations handle.
    pub fn new(git: GitOps) -> Self {
        Self { git }
    }

    /// Borrow the inner `GitOps` for inspection.
    pub fn git(&self) -> &GitOps {
        &self.git
    }

    /// Attempt to materialize a changeset to trunk.
    ///
    /// `upper_dir` is the agent's overlay upper directory containing modified
    /// files. The materializer reads agent changes from there, runs semantic
    /// merge checks if trunk has advanced, and either commits the result or
    /// reports conflicts.
    pub async fn materialize(
        &self,
        changeset: &Changeset,
        upper_dir: &Path,
        event_store: &dyn EventStore,
        analyzer: &dyn SemanticAnalyzer,
        message: &str,
    ) -> Result<MaterializeResult, OrchestratorError> {
        let head = self.git.head_oid()?;
        let trunk_path = self
            .git
            .repo()
            .workdir()
            .ok_or_else(|| {
                OrchestratorError::NotFound("repository has no working directory".into())
            })?
            .to_path_buf();

        if head == changeset.base_commit {
            return self
                .direct_apply(changeset, upper_dir, &trunk_path, message, event_store)
                .await;
        }

        let ctx = MergeContext {
            upper_dir,
            trunk_path: &trunk_path,
            head: &head,
            message,
            event_store,
            analyzer,
        };
        self.merge_apply(changeset, &ctx).await
    }

    /// Fast path: trunk hasn't moved, apply overlay directly.
    async fn direct_apply(
        &self,
        changeset: &Changeset,
        upper_dir: &Path,
        trunk_path: &Path,
        message: &str,
        event_store: &dyn EventStore,
    ) -> Result<MaterializeResult, OrchestratorError> {
        debug!(changeset = %changeset.id, "direct apply — trunk has not advanced");

        let new_commit = self.git.commit_overlay_changes(
            upper_dir,
            trunk_path,
            message,
            &changeset.agent_id.0,
        )?;

        self.append_materialized_event(changeset, &new_commit, event_store)
            .await?;

        Ok(MaterializeResult::Success { new_commit })
    }

    /// Slow path: trunk advanced, run three-way semantic merge per file.
    async fn merge_apply(
        &self,
        changeset: &Changeset,
        ctx: &MergeContext<'_>,
    ) -> Result<MaterializeResult, OrchestratorError> {
        debug!(
            changeset = %changeset.id,
            base = %changeset.base_commit,
            head = %ctx.head,
            "trunk advanced — running semantic merge"
        );

        let mut all_conflicts = Vec::new();
        let mut merged_files: Vec<(PathBuf, Vec<u8>)> = Vec::new();

        // Group the agent's submitted operations by file for symbol-level
        // pre-check. When the agent's changes and trunk's changes affect
        // disjoint symbols, we skip the expensive semantic three-way merge
        // and use the faster text-based merge instead.
        let agent_ops_by_file = group_ops_by_file(&changeset.operations);

        for file in &changeset.files_touched {
            self.validate_path(file, ctx.trunk_path)?;

            let theirs_path = ctx.upper_dir.join(file);

            // File in agent's overlay — read the agent's version
            let theirs = if theirs_path.exists() {
                std::fs::read(&theirs_path)?
            } else {
                // Agent deleted this file (or it's a whiteout)
                continue;
            };

            // Check if the file existed at the base commit
            let base = match self.git.read_file_at_commit(&changeset.base_commit, file) {
                Ok(content) => Some(content),
                Err(OrchestratorError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };

            match base {
                None => {
                    // New file — didn't exist at base. Check if it appeared on trunk since.
                    match self.git.read_file_at_commit(ctx.head, file) {
                        Ok(ours) => {
                            // File was added on trunk too — need merge with empty base
                            let result = ctx
                                .analyzer
                                .three_way_merge(&[], &ours, &theirs, file)
                                .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;
                            match result {
                                MergeResult::Clean(content) => {
                                    merged_files.push((file.clone(), content));
                                }
                                MergeResult::Conflict(conflicts) => {
                                    all_conflicts.extend(conflicts);
                                }
                            }
                        }
                        Err(OrchestratorError::NotFound(_)) => {
                            // New file not on trunk either — just add it
                            merged_files.push((file.clone(), theirs));
                        }
                        Err(e) => return Err(e),
                    }
                }
                Some(base_content) => {
                    // File existed at base — read trunk's current version
                    let ours = match self.git.read_file_at_commit(ctx.head, file) {
                        Ok(content) => content,
                        Err(OrchestratorError::NotFound(_)) => {
                            // File was deleted on trunk since base
                            all_conflicts.push(ConflictDetail {
                                kind: phantom_core::conflict::ConflictKind::ModifyDeleteSymbol,
                                file: file.clone(),
                                symbol_id: None,
                                ours_changeset: phantom_core::id::ChangesetId("trunk".into()),
                                theirs_changeset: changeset.id.clone(),
                                description: format!(
                                    "file {} was deleted on trunk but modified by agent",
                                    file.display()
                                ),
                                ours_span: None,
                                theirs_span: None,
                                base_span: None,
                            });
                            continue;
                        }
                        Err(e) => return Err(e),
                    };

                    // If trunk version hasn't changed from base, no merge needed
                    if ours == base_content {
                        merged_files.push((file.clone(), theirs));
                        continue;
                    }

                    // Use submitted operations for symbol-level pre-check.
                    // When the agent's operations for this file don't overlap
                    // with trunk's symbol changes, we can skip the expensive
                    // semantic three-way merge and use a faster text merge.
                    let symbols_disjoint = if let Some(agent_syms) = agent_ops_by_file.get(file) {
                        if agent_syms.is_empty() {
                            false
                        } else {
                            let base_syms = ctx
                                .analyzer
                                .extract_symbols(file, &base_content)
                                .unwrap_or_default();
                            let trunk_syms = ctx
                                .analyzer
                                .extract_symbols(file, &ours)
                                .unwrap_or_default();
                            let trunk_ops = ctx.analyzer.diff_symbols(&base_syms, &trunk_syms);
                            let trunk_names: HashSet<String> = trunk_ops
                                .iter()
                                .filter_map(|op| op.symbol_name().map(String::from))
                                .collect();
                            !agent_syms.iter().any(|s| trunk_names.contains(s.as_str()))
                        }
                    } else {
                        false
                    };

                    if symbols_disjoint {
                        debug!(file = %file.display(), "symbol-disjoint — using text merge");
                        if let MergeResult::Clean(content) =
                            self.git.text_merge(&base_content, &ours, &theirs)?
                        {
                            merged_files.push((file.clone(), content));
                            continue;
                        }
                        // Text merge failed despite disjoint symbols (e.g.
                        // additions at the exact same line). Fall through to
                        // semantic merge which may resolve it.
                        debug!(
                            file = %file.display(),
                            "text merge conflict despite disjoint symbols, falling back to semantic merge"
                        );
                    }

                    // Three-way semantic merge
                    let result = ctx
                        .analyzer
                        .three_way_merge(&base_content, &ours, &theirs, file)
                        .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;

                    match result {
                        MergeResult::Clean(content) => {
                            merged_files.push((file.clone(), content));
                        }
                        MergeResult::Conflict(conflicts) => {
                            all_conflicts.extend(conflicts);
                        }
                    }
                }
            }
        }

        if !all_conflicts.is_empty() {
            self.append_conflicted_event(changeset, &all_conflicts, ctx.event_store)
                .await?;
            return Ok(MaterializeResult::Conflict {
                details: all_conflicts,
            });
        }

        // Write merged files to the working tree and commit
        for (file, content) in &merged_files {
            let dst = ctx.trunk_path.join(file);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dst, content)?;
        }

        // Stage and commit, using the verified head OID as parent
        let file_paths: Vec<_> = merged_files.iter().map(|(p, _)| p.clone()).collect();
        let new_commit =
            self.commit_merged_files(&file_paths, ctx.head, ctx.message, &changeset.agent_id.0)?;

        self.append_materialized_event(changeset, &new_commit, ctx.event_store)
            .await?;

        Ok(MaterializeResult::Success { new_commit })
    }

    /// Validate that a relative path does not escape the trunk directory.
    fn validate_path(&self, file: &Path, trunk_path: &Path) -> Result<(), OrchestratorError> {
        // Reject absolute paths — joining an absolute path replaces the base entirely
        if file.is_absolute() {
            return Err(OrchestratorError::MaterializationFailed(format!(
                "path must be relative, got absolute: {}",
                file.display()
            )));
        }

        // Reject paths with parent traversal components
        for component in file.components() {
            if matches!(component, std::path::Component::ParentDir) {
                return Err(OrchestratorError::MaterializationFailed(format!(
                    "path contains parent traversal (..): {}",
                    file.display()
                )));
            }
        }

        // Final check: the joined path must still start with trunk_path
        let joined = trunk_path.join(file);
        if !joined.starts_with(trunk_path) {
            return Err(OrchestratorError::MaterializationFailed(format!(
                "path escapes working tree: {}",
                file.display()
            )));
        }

        Ok(())
    }

    /// Stage the given files and create a commit with a specific parent.
    ///
    /// Uses the provided `parent_oid` rather than re-fetching HEAD to avoid
    /// TOCTOU races between the merge check and the commit.
    fn commit_merged_files(
        &self,
        files: &[PathBuf],
        parent_oid: &GitOid,
        message: &str,
        author: &str,
    ) -> Result<GitOid, OrchestratorError> {
        let mut index = self.git.repo().index()?;
        for file in files {
            index.add_path(file)?;
        }
        index.write()?;

        let tree_oid = index.write_tree()?;
        let tree = self.git.repo().find_tree(tree_oid)?;
        let sig = git2::Signature::now(author, &format!("{author}@phantom"))?;

        let git2_parent_oid = git::git_oid_to_oid(parent_oid)?;
        let parent = self.git.repo().find_commit(git2_parent_oid)?;

        let new_oid =
            self.git
                .repo()
                .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?;

        Ok(git::oid_to_git_oid(new_oid))
    }

    /// Append a `ChangesetMaterialized` event to the store.
    async fn append_materialized_event(
        &self,
        changeset: &Changeset,
        new_commit: &GitOid,
        event_store: &dyn EventStore,
    ) -> Result<(), OrchestratorError> {
        let event = Event {
            id: EventId(0), // assigned by store
            timestamp: Utc::now(),
            changeset_id: changeset.id.clone(),
            agent_id: changeset.agent_id.clone(),
            kind: EventKind::ChangesetMaterialized {
                new_commit: *new_commit,
            },
        };
        event_store
            .append(event)
            .await
            .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
        Ok(())
    }

    /// Append a `ChangesetConflicted` event to the store.
    async fn append_conflicted_event(
        &self,
        changeset: &Changeset,
        conflicts: &[ConflictDetail],
        event_store: &dyn EventStore,
    ) -> Result<(), OrchestratorError> {
        let event = Event {
            id: EventId(0), // assigned by store
            timestamp: Utc::now(),
            changeset_id: changeset.id.clone(),
            agent_id: changeset.agent_id.clone(),
            kind: EventKind::ChangesetConflicted {
                conflicts: conflicts.to_vec(),
            },
        };
        event_store
            .append(event)
            .await
            .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
        Ok(())
    }
}

/// Group semantic operations by file path, collecting the symbol names
/// modified in each file. Used by the materializer to compare the agent's
/// submitted operations against trunk changes for a fast symbol-level
/// overlap check.
fn group_ops_by_file(operations: &[SemanticOperation]) -> HashMap<PathBuf, HashSet<String>> {
    let mut map: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for op in operations {
        if let Some(name) = op.symbol_name() {
            map.entry(op.file_path().to_path_buf())
                .or_default()
                .insert(name.to_string());
        }
    }
    map
}

/// Bundled context for a merge-apply operation, avoiding excessive parameter counts.
struct MergeContext<'a> {
    upper_dir: &'a Path,
    trunk_path: &'a Path,
    head: &'a GitOid,
    message: &'a str,
    event_store: &'a dyn EventStore,
    analyzer: &'a dyn SemanticAnalyzer,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::RwLock;

    use chrono::{DateTime, Utc};
    use phantom_core::changeset::{Changeset, ChangesetStatus};
    use phantom_core::conflict::ConflictDetail;
    use phantom_core::error::CoreError;
    use phantom_core::event::Event;
    use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
    use phantom_core::symbol::SymbolEntry;
    use phantom_core::traits::{EventStore, MergeResult, SemanticAnalyzer};

    // -----------------------------------------------------------------------
    // Mock EventStore
    // -----------------------------------------------------------------------

    struct MockEventStore {
        events: RwLock<Vec<Event>>,
    }

    impl MockEventStore {
        fn new() -> Self {
            Self {
                events: RwLock::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<Event> {
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

    // -----------------------------------------------------------------------
    // Mock SemanticAnalyzer
    // -----------------------------------------------------------------------

    struct MockAnalyzer {
        merge_results: HashMap<PathBuf, MergeResult>,
    }

    impl MockAnalyzer {
        fn new() -> Self {
            Self {
                merge_results: HashMap::new(),
            }
        }

        fn set_merge_result(&mut self, path: PathBuf, result: MergeResult) {
            self.merge_results.insert(path, result);
        }
    }

    impl SemanticAnalyzer for MockAnalyzer {
        fn extract_symbols(
            &self,
            _path: &Path,
            _content: &[u8],
        ) -> Result<Vec<SymbolEntry>, CoreError> {
            Ok(vec![])
        }

        fn diff_symbols(
            &self,
            _base: &[SymbolEntry],
            _current: &[SymbolEntry],
        ) -> Vec<phantom_core::changeset::SemanticOperation> {
            vec![]
        }

        fn three_way_merge(
            &self,
            _base: &[u8],
            _ours: &[u8],
            _theirs: &[u8],
            path: &Path,
        ) -> Result<MergeResult, CoreError> {
            match self.merge_results.get(path) {
                Some(result) => Ok(result.clone()),
                None => Ok(MergeResult::Clean(b"default merged content".to_vec())),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Create a temporary git repo with an initial commit.
    fn init_repo(files: &[(&str, &[u8])]) -> (tempfile::TempDir, GitOps) {
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
    fn advance_trunk(git: &GitOps, files: &[(&str, &[u8])]) -> GitOid {
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
    fn make_upper(files: &[(&str, &[u8])]) -> tempfile::TempDir {
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

    fn make_changeset(id: &str, base: GitOid, files: Vec<PathBuf>) -> Changeset {
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

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn direct_apply_trunk_not_advanced() {
        let (_dir, git) = init_repo(&[("src/main.rs", b"fn main() {}")]);
        let base = git.head_oid().unwrap();
        let upper = make_upper(&[("src/main.rs", b"fn main() { println!(\"hi\"); }")]);
        let event_store = MockEventStore::new();
        let analyzer = MockAnalyzer::new();

        let changeset = make_changeset("cs-1", base, vec![PathBuf::from("src/main.rs")]);

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit")
            .await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit } => {
                assert_ne!(new_commit, base);
                let events = event_store.events();
                assert_eq!(events.len(), 1);
                match &events[0].kind {
                    EventKind::ChangesetMaterialized { new_commit: nc } => {
                        assert_eq!(*nc, new_commit);
                    }
                    other => panic!("expected ChangesetMaterialized, got {other:?}"),
                }
            }
            MaterializeResult::Conflict { .. } => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn clean_merge_trunk_advanced() {
        let (_dir, git) =
            init_repo(&[("src/api.rs", b"fn api() {}"), ("src/db.rs", b"fn db() {}")]);
        let base = git.head_oid().unwrap();

        advance_trunk(&git, &[("src/db.rs", b"fn db() { /* updated */ }")]);

        let upper = make_upper(&[("src/api.rs", b"fn api() { /* agent changes */ }")]);
        let event_store = MockEventStore::new();
        let analyzer = MockAnalyzer::new();

        let changeset = make_changeset("cs-2", base, vec![PathBuf::from("src/api.rs")]);

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit")
            .await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit } => {
                assert_ne!(new_commit, base);
                let events = event_store.events();
                assert_eq!(events.len(), 1);
                assert!(matches!(
                    &events[0].kind,
                    EventKind::ChangesetMaterialized { .. }
                ));
            }
            MaterializeResult::Conflict { .. } => panic!("expected clean merge"),
        }
    }

    #[tokio::test]
    async fn conflict_detected() {
        let (_dir, git) = init_repo(&[("src/lib.rs", b"fn original() {}")]);
        let base = git.head_oid().unwrap();

        advance_trunk(&git, &[("src/lib.rs", b"fn trunk_version() {}")]);

        let upper = make_upper(&[("src/lib.rs", b"fn agent_version() {}")]);
        let event_store = MockEventStore::new();

        let mut analyzer = MockAnalyzer::new();
        analyzer.set_merge_result(
            PathBuf::from("src/lib.rs"),
            MergeResult::Conflict(vec![ConflictDetail {
                kind: phantom_core::conflict::ConflictKind::BothModifiedSymbol,
                file: PathBuf::from("src/lib.rs"),
                symbol_id: None,
                ours_changeset: ChangesetId("trunk".into()),
                theirs_changeset: ChangesetId("cs-3".into()),
                description: "both modified same symbol".into(),
                ours_span: None,
                theirs_span: None,
                base_span: None,
            }]),
        );

        let changeset = make_changeset("cs-3", base, vec![PathBuf::from("src/lib.rs")]);

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit").await
            .unwrap();

        match result {
            MaterializeResult::Conflict { details } => {
                assert_eq!(details.len(), 1);
                assert_eq!(
                    details[0].kind,
                    phantom_core::conflict::ConflictKind::BothModifiedSymbol
                );
                let events = event_store.events();
                assert_eq!(events.len(), 1);
                assert!(matches!(
                    &events[0].kind,
                    EventKind::ChangesetConflicted { .. }
                ));
            }
            MaterializeResult::Success { .. } => panic!("expected conflict"),
        }
    }

    #[tokio::test]
    async fn new_file_not_in_base() {
        let (_dir, git) = init_repo(&[("src/main.rs", b"fn main() {}")]);
        let base = git.head_oid().unwrap();

        advance_trunk(&git, &[("src/main.rs", b"fn main() { /* updated */ }")]);

        let upper = make_upper(&[("src/new_module.rs", b"pub fn new_thing() {}")]);
        let event_store = MockEventStore::new();
        let analyzer = MockAnalyzer::new();

        let changeset = make_changeset("cs-4", base, vec![PathBuf::from("src/new_module.rs")]);

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit").await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit } => {
                let content = materializer
                    .git
                    .read_file_at_commit(&new_commit, Path::new("src/new_module.rs"))
                    .unwrap();
                assert_eq!(content, b"pub fn new_thing() {}");
            }
            MaterializeResult::Conflict { .. } => panic!("expected success for new file"),
        }
    }

    #[tokio::test]
    async fn multiple_files_partial_conflict() {
        let (_dir, git) = init_repo(&[
            ("file_a.rs", b"fn a() {}"),
            ("file_b.rs", b"fn b() {}"),
            ("file_c.rs", b"fn c() {}"),
        ]);
        let base = git.head_oid().unwrap();

        advance_trunk(
            &git,
            &[
                ("file_a.rs", b"fn a() { /* trunk */ }"),
                ("file_b.rs", b"fn b() { /* trunk */ }"),
                ("file_c.rs", b"fn c() { /* trunk */ }"),
            ],
        );

        let upper = make_upper(&[
            ("file_a.rs", b"fn a() { /* agent */ }"),
            ("file_b.rs", b"fn b() { /* agent */ }"),
            ("file_c.rs", b"fn c() { /* agent */ }"),
        ]);
        let event_store = MockEventStore::new();

        let mut analyzer = MockAnalyzer::new();
        analyzer.set_merge_result(
            PathBuf::from("file_a.rs"),
            MergeResult::Clean(b"fn a() { /* merged */ }".to_vec()),
        );
        analyzer.set_merge_result(
            PathBuf::from("file_b.rs"),
            MergeResult::Conflict(vec![ConflictDetail {
                kind: phantom_core::conflict::ConflictKind::BothModifiedSymbol,
                file: PathBuf::from("file_b.rs"),
                symbol_id: None,
                ours_changeset: ChangesetId("trunk".into()),
                theirs_changeset: ChangesetId("cs-5".into()),
                description: "conflict in file_b".into(),
                ours_span: None,
                theirs_span: None,
                base_span: None,
            }]),
        );
        analyzer.set_merge_result(
            PathBuf::from("file_c.rs"),
            MergeResult::Clean(b"fn c() { /* merged */ }".to_vec()),
        );

        let changeset = make_changeset(
            "cs-5",
            base,
            vec![
                PathBuf::from("file_a.rs"),
                PathBuf::from("file_b.rs"),
                PathBuf::from("file_c.rs"),
            ],
        );

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit").await
            .unwrap();

        match result {
            MaterializeResult::Conflict { details } => {
                assert_eq!(details.len(), 1);
                assert_eq!(details[0].file, PathBuf::from("file_b.rs"));
            }
            MaterializeResult::Success { .. } => {
                panic!("expected conflict due to file_b")
            }
        }
    }

    #[tokio::test]
    async fn trunk_unchanged_file_uses_agent_version_directly() {
        let (_dir, git) = init_repo(&[
            ("src/api.rs", b"fn api() {}"),
            ("src/other.rs", b"fn other() {}"),
        ]);
        let base = git.head_oid().unwrap();

        advance_trunk(&git, &[("src/other.rs", b"fn other() { /* new */ }")]);

        let upper = make_upper(&[("src/api.rs", b"fn api() { /* agent */ }")]);
        let event_store = MockEventStore::new();
        let analyzer = MockAnalyzer::new();

        let changeset = make_changeset("cs-6", base, vec![PathBuf::from("src/api.rs")]);

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit").await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit } => {
                let content = materializer
                    .git
                    .read_file_at_commit(&new_commit, Path::new("src/api.rs"))
                    .unwrap();
                assert_eq!(content, b"fn api() { /* agent */ }");
            }
            MaterializeResult::Conflict { .. } => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let (_dir, git) = init_repo(&[("src/main.rs", b"fn main() {}")]);
        let base = git.head_oid().unwrap();

        advance_trunk(&git, &[("src/main.rs", b"fn main() { /* v2 */ }")]);

        let upper = make_upper(&[("src/main.rs", b"fn main() { /* agent */ }")]);
        let event_store = MockEventStore::new();
        let analyzer = MockAnalyzer::new();

        let changeset = make_changeset("cs-bad", base, vec![PathBuf::from("../../../etc/passwd")]);

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit")
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("parent traversal"), "error was: {err}");
    }

    #[tokio::test]
    async fn rejects_absolute_path() {
        let (_dir, git) = init_repo(&[("src/main.rs", b"fn main() {}")]);
        let base = git.head_oid().unwrap();

        advance_trunk(&git, &[("src/main.rs", b"fn main() { /* v2 */ }")]);

        let upper = make_upper(&[("src/main.rs", b"fn main() { /* agent */ }")]);
        let event_store = MockEventStore::new();
        let analyzer = MockAnalyzer::new();

        let changeset = make_changeset("cs-abs", base, vec![PathBuf::from("/etc/passwd")]);

        let materializer = Materializer::new(git);
        let result = materializer
            .materialize(&changeset, upper.path(), &event_store, &analyzer, "test commit")
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("absolute"), "error was: {err}");
    }
}
