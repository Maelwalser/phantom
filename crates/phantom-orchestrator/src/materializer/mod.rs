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

use std::path::{Path, PathBuf};

use phantom_core::changeset::Changeset;
use phantom_core::conflict::ConflictDetail;
use phantom_core::id::GitOid;
use phantom_core::traits::{EventStore, SemanticAnalyzer};

use crate::error::OrchestratorError;
use crate::git::GitOps;

mod commit;
mod direct_apply;
mod events;
mod lock;
mod merge_apply;
mod merge_file;
mod path_validation;

use lock::MaterializeLock;
use merge_apply::MergeContext;

/// Result of a materialization attempt.
#[derive(Debug)]
pub enum MaterializeResult {
    /// The changeset was successfully committed to trunk.
    Success {
        /// The new trunk commit OID.
        new_commit: GitOid,
        /// Files that were merged via line-based text fallback because no
        /// tree-sitter grammar is available for their language. These files
        /// had no syntax validation after merging.
        text_fallback_files: Vec<PathBuf>,
    },
    /// The changeset had conflicts and was not committed.
    Conflict {
        /// Details of each conflict found.
        details: Vec<ConflictDetail>,
    },
}

/// Coordinates changeset materialization to trunk.
pub struct Materializer<'a> {
    git: &'a GitOps,
}

impl<'a> Materializer<'a> {
    /// Create a materializer backed by the given git operations handle.
    pub fn new(git: &'a GitOps) -> Self {
        Self { git }
    }

    /// Borrow the inner `GitOps` for inspection.
    pub fn git(&self) -> &GitOps {
        self.git
    }

    /// Attempt to materialize a changeset to trunk.
    ///
    /// `upper_dir` is the agent's overlay upper directory containing modified
    /// files. The materializer reads agent changes from there, runs semantic
    /// merge checks if trunk has advanced, and either commits the result or
    /// reports conflicts.
    ///
    /// `phantom_dir` is used to acquire a file-based exclusive lock so that
    /// concurrent submits are serialized. Pass `None` in tests that don't
    /// need locking.
    pub async fn materialize(
        &self,
        changeset: &Changeset,
        upper_dir: &Path,
        event_store: &dyn EventStore,
        analyzer: &dyn SemanticAnalyzer,
        message: &str,
        phantom_dir: Option<&Path>,
    ) -> Result<MaterializeResult, OrchestratorError> {
        // Acquire exclusive lock to prevent concurrent materializations from
        // orphaning commits (C5).
        let _lock = phantom_dir.map(MaterializeLock::acquire).transpose()?;

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
            return direct_apply::direct_apply(
                self.git,
                changeset,
                upper_dir,
                &head,
                message,
                event_store,
            )
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
        merge_apply::merge_apply(self.git, changeset, &ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::path::Path;

    use phantom_core::conflict::ConflictDetail;
    use phantom_core::conflict::MergeResult;
    use phantom_core::error::CoreError;
    use phantom_core::event::EventKind;
    use phantom_core::id::ChangesetId;
    use phantom_core::symbol::SymbolEntry;

    use crate::test_support::{
        MockEventStore, advance_trunk, init_repo, make_changeset, make_upper,
    };

    // ---------------------------------------------------------------------------
    // Mock SemanticAnalyzer (materializer-specific: configurable merge results)
    // ---------------------------------------------------------------------------

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

    impl phantom_core::traits::SemanticAnalyzer for MockAnalyzer {
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

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn direct_apply_trunk_not_advanced() {
        let (_dir, git) = init_repo(&[("src/main.rs", b"fn main() {}")]);
        let base = git.head_oid().unwrap();
        let upper = make_upper(&[("src/main.rs", b"fn main() { println!(\"hi\"); }")]);
        let event_store = MockEventStore::new();
        let analyzer = MockAnalyzer::new();

        let changeset = make_changeset("cs-1", base, vec![PathBuf::from("src/main.rs")]);

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
            .await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit, .. } => {
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

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
            .await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit, .. } => {
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

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
            .await
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

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
            .await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit, .. } => {
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

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
            .await
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

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
            .await
            .unwrap();

        match result {
            MaterializeResult::Success { new_commit, .. } => {
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

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
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

        let materializer = Materializer::new(&git);
        let result = materializer
            .materialize(
                &changeset,
                upper.path(),
                &event_store,
                &analyzer,
                "test commit",
                None,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("absolute"), "error was: {err}");
    }
}
