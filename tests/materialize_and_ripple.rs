//! Integration test: after materialization, the ripple checker identifies
//! which active agents are affected by the trunk change.

mod common;

use std::path::PathBuf;

use phantom_core::id::AgentId;
use phantom_orchestrator::materializer::MaterializeResult;
use phantom_orchestrator::ripple::RippleChecker;

use crate::common::TestContext;

#[test]
fn test_ripple_notification_after_materialize() {
    let ctx = TestContext::new();

    // Seed trunk with a shared file.
    let base = ctx.commit_files(&[
        ("src/shared.rs", "fn helper() {}\n"),
    ]);

    // Agent-a adds a new function to shared.rs.
    let (agent_a, upper_a) = ctx.create_agent("agent-a", &[
        ("src/shared.rs", "fn helper() {}\nfn new_func() {}\n"),
    ]);

    let cs_a = ctx.build_changeset(
        "cs-a",
        &agent_a,
        base,
        vec![PathBuf::from("src/shared.rs")],
        "add new_func",
    );

    let old_head = ctx.head();

    // Materialize agent-a.
    let mat = ctx.materializer();
    let result = mat
        .materialize(&cs_a, upper_a.path(), &ctx.events, &ctx.merger)
        .expect("materialize failed");

    let new_head = match result {
        MaterializeResult::Success { new_commit } => new_commit,
        MaterializeResult::Conflict { details } => {
            panic!("expected success, got conflict: {details:?}")
        }
    };

    // Determine which files changed between old and new HEAD.
    let mat2 = ctx.materializer();
    let changed_files = mat2
        .git()
        .changed_files(&old_head, &new_head)
        .expect("changed_files failed");

    assert!(
        changed_files.contains(&PathBuf::from("src/shared.rs")),
        "src/shared.rs should be in changed files"
    );

    // Simulate agent-b working on src/shared.rs and agent-c on an unrelated file.
    let active_agents = vec![
        (
            AgentId("agent-b".into()),
            vec![PathBuf::from("src/shared.rs"), PathBuf::from("src/other.rs")],
        ),
        (AgentId("agent-c".into()), vec![PathBuf::from("src/unrelated.rs")]),
    ];

    let ripple = RippleChecker::check_ripple(&changed_files, &active_agents);

    // Agent-b should be affected (touches shared.rs).
    assert!(
        ripple.contains_key(&AgentId("agent-b".into())),
        "agent-b should be in ripple results"
    );
    let agent_b_affected = &ripple[&AgentId("agent-b".into())];
    assert!(
        agent_b_affected.contains(&PathBuf::from("src/shared.rs")),
        "agent-b's affected files should include src/shared.rs"
    );

    // Agent-c should NOT be affected (unrelated file).
    assert!(
        !ripple.contains_key(&AgentId("agent-c".into())),
        "agent-c should not be in ripple results"
    );
}

#[test]
fn test_overlay_lower_layer_reflects_new_trunk() {
    use phantom_overlay::layer::OverlayLayer;

    let ctx = TestContext::new();
    let trunk_path = ctx.git.repo().workdir().unwrap().to_path_buf();

    // Seed trunk with a file.
    let _base = ctx.commit_files(&[
        ("src/data.rs", "fn original() {}\n"),
    ]);

    // Create an overlay layer for agent-b pointing at current trunk.
    let upper_dir = tempfile::TempDir::new().unwrap();
    let mut layer = OverlayLayer::new(trunk_path.clone(), upper_dir.path().to_path_buf())
        .expect("failed to create overlay");

    // Agent-b can see the original file via lower layer.
    let content = layer
        .read_file(std::path::Path::new("src/data.rs"))
        .expect("should read through lower layer");
    assert_eq!(
        String::from_utf8_lossy(&content),
        "fn original() {}\n"
    );

    // Simulate trunk advancing (agent-a materializes).
    let _new_head = ctx.commit_files(&[
        ("src/data.rs", "fn original() {}\nfn added_by_a() {}\n"),
    ]);

    // Update the overlay's lower layer to the new trunk state.
    layer.update_lower(trunk_path);

    // Now agent-b's overlay should reflect the updated trunk.
    let updated = layer
        .read_file(std::path::Path::new("src/data.rs"))
        .expect("should read updated lower layer");
    let text = String::from_utf8_lossy(&updated);
    assert!(
        text.contains("added_by_a"),
        "overlay should reflect new trunk after update_lower"
    );
}
