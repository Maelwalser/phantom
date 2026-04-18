//! Regression test: phantom must not try to materialize against a null parent
//! OID when the repo has no initial commit. See the hivemind incident where
//! `ph plan` dispatched 8 agents on an unborn-HEAD repo and every one crashed
//! during materialization with a libgit2 `null OID cannot exist` error.
//!
//! The three guards under test:
//!   1. `write_current_base` refuses to persist the null OID.
//!   2. `commit_from_oids` (the materializer's parent lookup) refuses the
//!      null OID before it reaches libgit2.
//!   3. After `ph init` seeds an empty initial commit, the same operations
//!      succeed against the seeded commit.

use phantom_core::{AgentId, GitOid};
use phantom_orchestrator::live_rebase;
use tempfile::TempDir;

#[test]
fn write_current_base_rejects_null_oid_on_unborn_head() {
    let phantom_dir = TempDir::new().unwrap();
    let agent = AgentId("alpha".into());

    let err = live_rebase::write_current_base(phantom_dir.path(), &agent, &GitOid::zero())
        .expect_err("must reject null OID");

    let msg = err.to_string();
    assert!(
        msg.contains("null OID") || msg.contains("no initial commit"),
        "error should mention null OID / missing initial commit, got: {msg}"
    );

    let persisted = phantom_dir
        .path()
        .join("overlays")
        .join("alpha")
        .join("current_base");
    assert!(
        !persisted.exists(),
        "rejected write must not leave current_base on disk"
    );
}

#[test]
fn write_current_base_accepts_real_commit_after_seed() {
    let repo_dir = TempDir::new().unwrap();
    let repo = git2::Repository::init(repo_dir.path()).unwrap();

    let sig = git2::Signature::now("tester", "tester@tester").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let commit_oid = repo
        .commit(Some("HEAD"), &sig, &sig, "seed", &tree, &[])
        .unwrap();
    drop(tree);

    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(commit_oid.as_bytes());
    let seeded = GitOid::from_bytes(bytes);

    let phantom_dir = TempDir::new().unwrap();
    let agent = AgentId("beta".into());

    live_rebase::write_current_base(phantom_dir.path(), &agent, &seeded)
        .expect("real OID must be accepted");

    let persisted = std::fs::read_to_string(
        phantom_dir
            .path()
            .join("overlays")
            .join("beta")
            .join("current_base"),
    )
    .unwrap();
    assert_eq!(persisted.trim(), seeded.to_hex());
}
