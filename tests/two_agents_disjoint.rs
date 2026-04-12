//! Integration test: two agents modify disjoint files → both auto-merge.

mod common;

use std::path::PathBuf;

use phantom_orchestrator::materializer::MaterializeResult;

use crate::common::TestContext;

#[test]
fn test_two_agents_disjoint_files_auto_merges() {
    let ctx = TestContext::new();

    // Seed trunk with two files in separate modules.
    let base = ctx.commit_files(&[
        ("src/a.rs", "fn alpha() -> i32 { 1 }\n"),
        ("src/b.rs", "fn beta() -> i32 { 2 }\n"),
    ]);

    // Agent-a modifies src/a.rs — adds a new function.
    let (agent_a, upper_a) = ctx.create_agent(
        "agent-a",
        &[(
            "src/a.rs",
            "fn alpha() -> i32 { 1 }\nfn alpha_two() -> i32 { 12 }\n",
        )],
    );

    // Agent-b modifies src/b.rs — adds a new function.
    let (agent_b, upper_b) = ctx.create_agent(
        "agent-b",
        &[(
            "src/b.rs",
            "fn beta() -> i32 { 2 }\nfn beta_two() -> i32 { 22 }\n",
        )],
    );

    let cs_a = ctx.build_changeset(
        "cs-a",
        &agent_a,
        base,
        vec![PathBuf::from("src/a.rs")],
        "add alpha_two",
    );
    let cs_b = ctx.build_changeset(
        "cs-b",
        &agent_b,
        base,
        vec![PathBuf::from("src/b.rs")],
        "add beta_two",
    );

    // Materialize agent-a first.
    let mat = ctx.materializer();
    let result_a = mat
        .materialize(&cs_a, upper_a.path(), &ctx.events, &ctx.merger, "test commit")
        .expect("materialize agent-a failed");
    assert!(
        matches!(result_a, MaterializeResult::Success { .. }),
        "agent-a should succeed, got {result_a:?}"
    );

    // Materialize agent-b — different files, should also succeed.
    let mat2 = ctx.materializer();
    let result_b = mat2
        .materialize(&cs_b, upper_b.path(), &ctx.events, &ctx.merger, "test commit")
        .expect("materialize agent-b failed");
    assert!(
        matches!(result_b, MaterializeResult::Success { .. }),
        "agent-b should succeed (disjoint files), got {result_b:?}"
    );

    // Verify trunk contains both changes via git object store.
    let mat_git = ctx.materializer();
    let head = mat_git.git().head_oid().unwrap();
    let a_content = String::from_utf8(
        mat_git
            .git()
            .read_file_at_commit(&head, &PathBuf::from("src/a.rs"))
            .unwrap(),
    )
    .unwrap();
    let b_content = String::from_utf8(
        mat_git
            .git()
            .read_file_at_commit(&head, &PathBuf::from("src/b.rs"))
            .unwrap(),
    )
    .unwrap();

    assert!(
        a_content.contains("alpha"),
        "trunk src/a.rs should contain alpha"
    );
    assert!(
        a_content.contains("alpha_two"),
        "trunk src/a.rs should contain alpha_two"
    );
    assert!(
        b_content.contains("beta"),
        "trunk src/b.rs should contain beta"
    );
    assert!(
        b_content.contains("beta_two"),
        "trunk src/b.rs should contain beta_two"
    );
}
