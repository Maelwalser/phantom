use super::*;

use phantom_core::conflict::ConflictKind;
use phantom_core::traits::MergeResult;
use std::path::PathBuf;
use tempfile::TempDir;

use crate::test_support::commit_file;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a git repo with an empty initial commit in the given directory.
fn init_repo(dir: &Path) -> GitOps {
    let repo = git2::Repository::init(dir).unwrap();

    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    drop(tree);
    drop(repo);

    GitOps::open(dir).unwrap()
}

/// A test analyzer that delegates to the text-based merge in git.rs.
struct TextMergeAnalyzer;

impl SemanticAnalyzer for TextMergeAnalyzer {
    fn extract_symbols(
        &self,
        _path: &Path,
        _content: &[u8],
    ) -> Result<Vec<phantom_core::symbol::SymbolEntry>, phantom_core::CoreError> {
        Ok(vec![])
    }

    fn diff_symbols(
        &self,
        _base: &[phantom_core::symbol::SymbolEntry],
        _current: &[phantom_core::symbol::SymbolEntry],
    ) -> Vec<phantom_core::changeset::SemanticOperation> {
        vec![]
    }

    fn three_way_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        _path: &Path,
    ) -> Result<MergeResult, phantom_core::CoreError> {
        let base_s = String::from_utf8_lossy(base);
        let ours_s = String::from_utf8_lossy(ours);
        let theirs_s = String::from_utf8_lossy(theirs);
        match diffy::merge(&base_s, &ours_s, &theirs_s) {
            Ok(merged) => Ok(MergeResult::Clean(merged.into_bytes())),
            Err(_) => Ok(MergeResult::Conflict(vec![
                phantom_core::conflict::ConflictDetail {
                    kind: phantom_core::conflict::ConflictKind::RawTextConflict,
                    file: PathBuf::new(),
                    symbol_id: None,
                    ours_changeset: phantom_core::id::ChangesetId("trunk".into()),
                    theirs_changeset: phantom_core::id::ChangesetId("agent".into()),
                    description: "text merge conflict".into(),
                    ours_span: None,
                    theirs_span: None,
                    base_span: None,
                },
            ])),
        }
    }
}

// ---------------------------------------------------------------------------
// current_base persistence tests
// ---------------------------------------------------------------------------

#[test]
fn current_base_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let agent = AgentId("agent-a".into());
    let oid = GitOid::from_bytes([0xAB; 20]);

    write_current_base(tmp.path(), &agent, &oid).unwrap();
    let read = read_current_base(tmp.path(), &agent).unwrap();
    assert_eq!(read, Some(oid));
}

#[test]
fn current_base_missing_returns_none() {
    let tmp = TempDir::new().unwrap();
    let agent = AgentId("ghost".into());
    let result = read_current_base(tmp.path(), &agent).unwrap();
    assert_eq!(result, None);
}

#[test]
fn current_base_invalid_hex_errors() {
    let tmp = TempDir::new().unwrap();
    let agent = AgentId("agent-bad".into());
    let dir = tmp.path().join("overlays").join("agent-bad");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("current_base"), "not-valid-hex").unwrap();

    let result = read_current_base(tmp.path(), &agent);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// atomic_write_upper tests
// ---------------------------------------------------------------------------

#[test]
fn atomic_write_creates_file_and_leaves_no_tmp() {
    let tmp = TempDir::new().unwrap();
    let upper = tmp.path();
    let rel = PathBuf::from("src/merged.rs");

    atomic_write_upper(upper, &rel, b"merged content").unwrap();

    assert_eq!(
        std::fs::read_to_string(upper.join("src/merged.rs")).unwrap(),
        "merged content"
    );
    assert!(!upper.join("src/merged.phantom-rebase-tmp").exists());
}

#[test]
fn atomic_write_overwrites_existing() {
    let tmp = TempDir::new().unwrap();
    let upper = tmp.path();
    let rel = PathBuf::from("file.rs");

    std::fs::write(upper.join("file.rs"), b"old content").unwrap();
    atomic_write_upper(upper, &rel, b"new content").unwrap();

    assert_eq!(
        std::fs::read_to_string(upper.join("file.rs")).unwrap(),
        "new content"
    );
}

// ---------------------------------------------------------------------------
// rebase_agent tests (require a real git repo)
// ---------------------------------------------------------------------------

#[test]
fn rebase_clean_merge_updates_upper() {
    let repo_dir = TempDir::new().unwrap();
    let upper_dir = TempDir::new().unwrap();
    let git = init_repo(repo_dir.path());
    let analyzer = TextMergeAnalyzer;
    let agent = AgentId("agent-b".into());

    let base = commit_file(
        &git,
        "src/shared.rs",
        b"// top\nfn alpha() {}\n// middle\nfn beta() {}\n// bottom\n",
        "base",
    );

    let head = commit_file(
        &git,
        "src/shared.rs",
        b"// top\nfn new_trunk() {}\nfn alpha() {}\n// middle\nfn beta() {}\n// bottom\n",
        "trunk adds fn",
    );

    std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
    std::fs::write(
        upper_dir.path().join("src/shared.rs"),
        b"// top\nfn alpha() {}\n// middle\nfn beta() {}\n// bottom\nfn agent_fn() {}\n",
    )
    .unwrap();

    let result = rebase_agent(
        &git,
        &analyzer,
        &agent,
        &base,
        &head,
        upper_dir.path(),
        &[PathBuf::from("src/shared.rs")],
    )
    .unwrap();

    assert_eq!(result.merged.len(), 1);
    assert!(result.conflicted.is_empty());

    let content = std::fs::read_to_string(upper_dir.path().join("src/shared.rs")).unwrap();
    assert!(content.contains("fn new_trunk()"));
    assert!(content.contains("fn agent_fn()"));
}

#[test]
fn rebase_conflict_leaves_upper_unchanged() {
    let repo_dir = TempDir::new().unwrap();
    let upper_dir = TempDir::new().unwrap();
    let git = init_repo(repo_dir.path());
    let analyzer = TextMergeAnalyzer;
    let agent = AgentId("agent-b".into());

    let base = commit_file(&git, "src/shared.rs", b"original\n", "base");
    let head = commit_file(&git, "src/shared.rs", b"trunk version\n", "trunk edit");

    std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
    std::fs::write(upper_dir.path().join("src/shared.rs"), b"agent version\n").unwrap();

    let result = rebase_agent(
        &git,
        &analyzer,
        &agent,
        &base,
        &head,
        upper_dir.path(),
        &[PathBuf::from("src/shared.rs")],
    )
    .unwrap();

    assert!(result.merged.is_empty());
    assert_eq!(result.conflicted.len(), 1);

    let content = std::fs::read_to_string(upper_dir.path().join("src/shared.rs")).unwrap();
    assert_eq!(content, "agent version\n");
}

#[test]
fn rebase_trunk_deleted_file_is_conflict() {
    let repo_dir = TempDir::new().unwrap();
    let upper_dir = TempDir::new().unwrap();
    let git = init_repo(repo_dir.path());
    let analyzer = TextMergeAnalyzer;
    let agent = AgentId("agent-b".into());

    let base = commit_file(&git, "src/gone.rs", b"content\n", "base");

    // Trunk: delete the file.
    let repo = git.repo();
    let workdir = repo.workdir().unwrap();
    std::fs::remove_file(workdir.join("src/gone.rs")).unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("src/gone.rs")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("test", "test@test.com").unwrap();
    let oid = repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "delete file",
            &tree,
            &[&head_commit],
        )
        .unwrap();
    let mut head_bytes = [0u8; 20];
    head_bytes.copy_from_slice(oid.as_bytes());
    let head = GitOid::from_bytes(head_bytes);

    std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
    std::fs::write(upper_dir.path().join("src/gone.rs"), b"agent modified\n").unwrap();

    let result = rebase_agent(
        &git,
        &analyzer,
        &agent,
        &base,
        &head,
        upper_dir.path(),
        &[PathBuf::from("src/gone.rs")],
    )
    .unwrap();

    assert!(result.merged.is_empty());
    assert_eq!(result.conflicted.len(), 1);
    assert_eq!(
        result.conflicted[0].1[0].kind,
        ConflictKind::ModifyDeleteSymbol
    );
}

#[test]
fn rebase_mixed_clean_and_conflict() {
    let repo_dir = TempDir::new().unwrap();
    let upper_dir = TempDir::new().unwrap();
    let git = init_repo(repo_dir.path());
    let analyzer = TextMergeAnalyzer;
    let agent = AgentId("agent-b".into());

    let _base1 = commit_file(
        &git,
        "src/clean.rs",
        b"// top\nfn alpha() {}\n// bottom\n",
        "base clean",
    );
    let base = commit_file(&git, "src/conflict.rs", b"original\n", "base conflict");

    let _mid = commit_file(
        &git,
        "src/clean.rs",
        b"// top\nfn trunk_fn() {}\nfn alpha() {}\n// bottom\n",
        "trunk clean",
    );
    let head = commit_file(
        &git,
        "src/conflict.rs",
        b"trunk version\n",
        "trunk conflict",
    );

    std::fs::create_dir_all(upper_dir.path().join("src")).unwrap();
    std::fs::write(
        upper_dir.path().join("src/clean.rs"),
        b"// top\nfn alpha() {}\n// bottom\nfn agent_fn() {}\n",
    )
    .unwrap();
    std::fs::write(upper_dir.path().join("src/conflict.rs"), b"agent version\n").unwrap();

    let result = rebase_agent(
        &git,
        &analyzer,
        &agent,
        &base,
        &head,
        upper_dir.path(),
        &[
            PathBuf::from("src/clean.rs"),
            PathBuf::from("src/conflict.rs"),
        ],
    )
    .unwrap();

    assert_eq!(result.merged.len(), 1);
    assert_eq!(result.conflicted.len(), 1);
    assert_eq!(result.merged[0], PathBuf::from("src/clean.rs"));
    assert_eq!(result.conflicted[0].0, PathBuf::from("src/conflict.rs"));
}

#[test]
fn rebase_empty_shadowed_list() {
    let repo_dir = TempDir::new().unwrap();
    let upper_dir = TempDir::new().unwrap();
    let git = init_repo(repo_dir.path());
    let analyzer = TextMergeAnalyzer;
    let agent = AgentId("agent-b".into());
    let head = git.head_oid().unwrap();

    let result =
        rebase_agent(&git, &analyzer, &agent, &head, &head, upper_dir.path(), &[]).unwrap();

    assert!(result.merged.is_empty());
    assert!(result.conflicted.is_empty());
}
