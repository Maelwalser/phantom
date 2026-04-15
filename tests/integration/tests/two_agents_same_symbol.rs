//! Integration test: two agents modify the same symbol → conflict detected.

use std::path::{Path, PathBuf};

use phantom_core::conflict::ConflictKind;
use phantom_orchestrator::materializer::MaterializeResult;
use phantom_testkit::TestContext;

#[tokio::test]
async fn test_two_agents_same_symbol_conflicts() {
    let ctx = TestContext::new_async().await;

    // Seed trunk with a lib file containing compute().
    let base = ctx.commit_files(&[("src/lib.rs", "fn compute() -> i32 { 42 }\n")]);

    // Agent-a changes compute() to return 100.
    let (agent_a, upper_a) = ctx.create_agent(
        "agent-a",
        &[("src/lib.rs", "fn compute() -> i32 { 100 }\n")],
    );

    // Agent-b changes compute() to return 200.
    let (agent_b, upper_b) = ctx.create_agent(
        "agent-b",
        &[("src/lib.rs", "fn compute() -> i32 { 200 }\n")],
    );

    let cs_a = ctx.build_changeset(
        "cs-a",
        &agent_a,
        base,
        vec![PathBuf::from("src/lib.rs")],
        "change compute to 100",
    );
    let cs_b = ctx.build_changeset(
        "cs-b",
        &agent_b,
        base,
        vec![PathBuf::from("src/lib.rs")],
        "change compute to 200",
    );

    // Materialize agent-a — should succeed (direct apply, trunk hasn't moved).
    let mat = ctx.materializer();
    let result_a = mat
        .materialize(
            &cs_a,
            upper_a.path(),
            &ctx.events,
            &ctx.merger,
            "test commit",
            None,
        )
        .await
        .expect("materialize agent-a failed");
    assert!(
        matches!(result_a, MaterializeResult::Success { .. }),
        "agent-a should succeed (first to materialize)"
    );

    // Materialize agent-b — both modified the same symbol → conflict.
    let mat2 = ctx.materializer();
    let result_b = mat2
        .materialize(
            &cs_b,
            upper_b.path(),
            &ctx.events,
            &ctx.merger,
            "test commit",
            None,
        )
        .await
        .expect("materialize agent-b failed");

    match result_b {
        MaterializeResult::Conflict { details } => {
            assert!(!details.is_empty(), "should have at least one conflict");

            // The conflict should involve BothModifiedSymbol or a text-level
            // conflict — either is acceptable since both agents modified the
            // same function body.
            let has_relevant_conflict = details.iter().any(|d| {
                matches!(
                    d.kind,
                    ConflictKind::BothModifiedSymbol | ConflictKind::RawTextConflict
                )
            });
            assert!(
                has_relevant_conflict,
                "expected BothModifiedSymbol or RawTextConflict, got {details:?}"
            );

            // Verify the conflict references the correct file.
            let references_lib = details.iter().any(|d| d.file == Path::new("src/lib.rs"));
            assert!(
                references_lib,
                "conflict should reference src/lib.rs, got {details:?}"
            );
        }
        MaterializeResult::Success { .. } => {
            panic!("expected conflict when both agents modify the same symbol")
        }
    }
}
