use super::*;

use std::collections::HashMap;
use std::path::Path;

use phantom_core::conflict::ConflictDetail;
use phantom_core::error::CoreError;
use phantom_core::id::ChangesetId;
use phantom_core::symbol::SymbolEntry;
use phantom_core::traits::MergeResult;

use crate::test_support::{advance_trunk, init_repo, make_changeset, make_upper, MockEventStore};

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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
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

    let materializer = Materializer::new(git);
    let result = materializer
        .materialize(
            &changeset,
            upper.path(),
            &event_store,
            &analyzer,
            "test commit",
        )
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("absolute"), "error was: {err}");
}
