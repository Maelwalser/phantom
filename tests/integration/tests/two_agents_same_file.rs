//! Integration test: two agents modify the same file but touch different
//! symbols → semantic merge auto-resolves.

use std::path::PathBuf;

use phantom_orchestrator::materializer::MaterializeResult;
use phantom_testkit::TestContext;

#[tokio::test]
async fn test_two_agents_same_file_different_symbols_auto_merges() {
    let ctx = TestContext::new_async().await;

    // Seed trunk with a handlers file containing one function.
    let base = ctx.commit_files(&[("src/handlers.rs", "fn handle_login() {}\n")]);

    // Agent-a adds handle_register.
    let (agent_a, upper_a) = ctx.create_agent(
        "agent-a",
        &[(
            "src/handlers.rs",
            "fn handle_login() {}\nfn handle_register() {}\n",
        )],
    );

    // Agent-b adds handle_admin.
    let (agent_b, upper_b) = ctx.create_agent(
        "agent-b",
        &[(
            "src/handlers.rs",
            "fn handle_login() {}\nfn handle_admin() {}\n",
        )],
    );

    let cs_a = ctx.build_changeset(
        "cs-a",
        &agent_a,
        base,
        vec![PathBuf::from("src/handlers.rs")],
        "add handle_register",
    );
    let cs_b = ctx.build_changeset(
        "cs-b",
        &agent_b,
        base,
        vec![PathBuf::from("src/handlers.rs")],
        "add handle_admin",
    );

    // Materialize agent-a.
    let mat = ctx.materializer();
    let result_a = mat
        .materialize(
            &cs_a,
            upper_a.path(),
            &ctx.events,
            &ctx.merger,
            "test commit",
        )
        .await
        .expect("materialize agent-a failed");
    assert!(
        matches!(result_a, MaterializeResult::Success { .. }),
        "agent-a should succeed, got {result_a:?}"
    );

    // Materialize agent-b — same file, different symbols.
    let mat2 = ctx.materializer();
    let result_b = mat2
        .materialize(
            &cs_b,
            upper_b.path(),
            &ctx.events,
            &ctx.merger,
            "test commit",
        )
        .await
        .expect("materialize agent-b failed");
    assert!(
        matches!(result_b, MaterializeResult::Success { .. }),
        "agent-b should succeed (different symbols), got {result_b:?}"
    );

    // Verify trunk handlers.rs contains all 3 functions.
    let mat_git = ctx.materializer();
    let head = mat_git.git().head_oid().unwrap();
    let content = String::from_utf8(
        mat_git
            .git()
            .read_file_at_commit(&head, &PathBuf::from("src/handlers.rs"))
            .unwrap(),
    )
    .unwrap();

    assert!(
        content.contains("handle_login"),
        "should contain original handle_login"
    );
    assert!(
        content.contains("handle_register"),
        "should contain agent-a's handle_register"
    );
    assert!(
        content.contains("handle_admin"),
        "should contain agent-b's handle_admin"
    );
}
