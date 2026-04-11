//! Integration test: rollback a middle changeset and replay downstream.

mod common;

use std::path::{Path, PathBuf};

use phantom_core::id::ChangesetId;
use phantom_core::traits::EventStore;
use phantom_events::ReplayEngine;
use phantom_orchestrator::materializer::MaterializeResult;

use crate::common::TestContext;

#[test]
fn test_rollback_middle_changeset_replays_downstream() {
    let ctx = TestContext::new();

    // Seed trunk with a base file.
    let base = ctx.commit_files(&[("src/lib.rs", "// base\n")]);

    // --- Materialize cs-001: adds fn one ---
    let (agent_1, upper_1) = ctx.create_agent("agent-1", &[
        ("src/lib.rs", "// base\nfn one() {}\n"),
    ]);
    let cs_001 = ctx.build_changeset(
        "cs-001",
        &agent_1,
        base,
        vec![PathBuf::from("src/lib.rs")],
        "add fn one",
    );
    let mat = ctx.materializer();
    let r1 = mat
        .materialize(&cs_001, upper_1.path(), &ctx.events, &ctx.merger)
        .unwrap();
    let commit_after_001 = match r1 {
        MaterializeResult::Success { new_commit } => new_commit,
        _ => panic!("cs-001 should succeed"),
    };

    // --- Materialize cs-002: adds fn two ---
    let (agent_2, upper_2) = ctx.create_agent("agent-2", &[
        ("src/lib.rs", "// base\nfn one() {}\nfn two() {}\n"),
    ]);
    let cs_002 = ctx.build_changeset(
        "cs-002",
        &agent_2,
        commit_after_001,
        vec![PathBuf::from("src/lib.rs")],
        "add fn two",
    );
    let mat2 = ctx.materializer();
    let r2 = mat2
        .materialize(&cs_002, upper_2.path(), &ctx.events, &ctx.merger)
        .unwrap();
    let commit_after_002 = match r2 {
        MaterializeResult::Success { new_commit } => new_commit,
        _ => panic!("cs-002 should succeed"),
    };

    // --- Materialize cs-003: adds fn three (independent of cs-002) ---
    let (agent_3, upper_3) = ctx.create_agent("agent-3", &[
        ("src/lib.rs", "// base\nfn one() {}\nfn two() {}\nfn three() {}\n"),
    ]);
    let cs_003 = ctx.build_changeset(
        "cs-003",
        &agent_3,
        commit_after_002,
        vec![PathBuf::from("src/lib.rs")],
        "add fn three",
    );
    let mat3 = ctx.materializer();
    let r3 = mat3
        .materialize(&cs_003, upper_3.path(), &ctx.events, &ctx.merger)
        .unwrap();
    assert!(
        matches!(r3, MaterializeResult::Success { .. }),
        "cs-003 should succeed"
    );

    // Verify all 3 functions exist before rollback.
    let mat_check = ctx.materializer();
    let head_before = mat_check.git().head_oid().unwrap();
    let content_before = String::from_utf8(
        mat_check
            .git()
            .read_file_at_commit(&head_before, Path::new("src/lib.rs"))
            .unwrap(),
    )
    .unwrap();
    assert!(content_before.contains("fn one()"));
    assert!(content_before.contains("fn two()"));
    assert!(content_before.contains("fn three()"));

    // --- Rollback cs-002 ---

    // 1. Use ReplayEngine to find changesets after cs-002.
    let engine = ReplayEngine::new(&ctx.events);
    let after_002 = engine
        .changesets_after(&ChangesetId("cs-002".into()))
        .unwrap();
    assert_eq!(after_002.len(), 1, "only cs-003 should be after cs-002");
    assert_eq!(after_002[0].0, "cs-003");

    // 2. Mark cs-002 events as dropped.
    let dropped = ctx.events.mark_dropped(&ChangesetId("cs-002".into())).unwrap();
    assert!(dropped > 0, "should have dropped at least one event");

    // 3. Reset trunk to commit_after_001 (before cs-002).
    let mat_reset = ctx.materializer();
    mat_reset
        .git()
        .reset_to_commit(&commit_after_001)
        .expect("reset failed");

    // 4. Re-materialize cs-003 against the post-rollback trunk.
    //    cs-003 added fn three, which is independent of fn two, so it should
    //    succeed. We re-build the changeset against the current HEAD.
    let current_head = mat_reset.git().head_oid().unwrap();
    let (agent_3b, upper_3b) = ctx.create_agent("agent-3b", &[
        ("src/lib.rs", "// base\nfn one() {}\nfn three() {}\n"),
    ]);
    let cs_003_replayed = ctx.build_changeset(
        "cs-003-replay",
        &agent_3b,
        current_head,
        vec![PathBuf::from("src/lib.rs")],
        "replay: add fn three",
    );
    let mat_replay = ctx.materializer();
    let r3_replay = mat_replay
        .materialize(
            &cs_003_replayed,
            upper_3b.path(),
            &ctx.events,
            &ctx.merger,
        )
        .unwrap();
    assert!(
        matches!(r3_replay, MaterializeResult::Success { .. }),
        "cs-003 replay should succeed (no dependency on cs-002)"
    );

    // 5. Verify trunk has fn one and fn three, but NOT fn two.
    let mat_final = ctx.materializer();
    let final_head = mat_final.git().head_oid().unwrap();
    let final_content = String::from_utf8(
        mat_final
            .git()
            .read_file_at_commit(&final_head, Path::new("src/lib.rs"))
            .unwrap(),
    )
    .unwrap();

    assert!(
        final_content.contains("fn one()"),
        "trunk should still have fn one"
    );
    assert!(
        final_content.contains("fn three()"),
        "trunk should have fn three (replayed)"
    );
    assert!(
        !final_content.contains("fn two()"),
        "trunk should NOT have fn two (rolled back)"
    );

    // 6. Verify cs-002 events are excluded from query.
    let cs2_events = ctx
        .events
        .query_by_changeset(&ChangesetId("cs-002".into()))
        .unwrap();
    assert!(
        cs2_events.is_empty(),
        "dropped changeset events should not appear in queries"
    );
}
