//! Integration test: if the `ChangesetMaterialized` event append fails
//! after the git commit lands, the pre-commit fence event must remain in
//! the event log. Recovery (`ph recover`) depends on the fence being
//! durable in exactly this window — without it, an orphan commit on trunk
//! would have no audit trail at all.
//!
//! This complements `materialize_append_crash.rs`, which pins the later
//! `ChangesetSubmitted` fault. Together they cover both append points.

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::error::OrchestratorError;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayLayer;
use phantom_testkit::TestContext;
use phantom_testkit::mocks::MockEventStore;

#[tokio::test]
async fn fence_survives_materialized_append_fault() {
    let ctx = TestContext::new_async().await;

    let base = ctx.commit_files(&[("src/lib.rs", "fn a() {}\n")]);
    let (agent_id, upper) =
        ctx.create_agent("agent-a", &[("src/lib.rs", "fn a() {}\nfn b() {}\n")]);

    let events = MockEventStore::new();
    events
        .append(Event {
            id: EventId(0),
            timestamp: chrono::Utc::now(),
            changeset_id: ChangesetId("cs-fault".into()),
            agent_id: agent_id.clone(),
            causal_parent: None,
            kind: EventKind::TaskCreated {
                base_commit: base,
                task: "add fn b".into(),
            },
        })
        .await
        .unwrap();

    // Fault at the materialized terminal. The fence append comes first
    // and must NOT be faulted — that's the point of this test.
    events.fail_when(|k| matches!(k, EventKind::ChangesetMaterialized { .. }));

    let trunk_path = ctx.git.repo().workdir().unwrap().to_path_buf();
    let layer = OverlayLayer::new(trunk_path, upper.path().to_path_buf()).unwrap();
    let phantom_dir = tempfile::TempDir::new().unwrap();

    let head_before = ctx.head();

    let result = submit_service::submit_and_materialize(
        &ctx.git,
        &events,
        &ctx.merger,
        &agent_id,
        &layer,
        upper.path(),
        phantom_dir.path(),
        &ctx.materializer(),
        &[],
        None,
    )
    .await;

    // 1. The fault propagates as an EventStore error.
    assert!(
        matches!(result, Err(OrchestratorError::EventStore(_))),
        "expected EventStore error, got: {result:?}"
    );

    // 2. HEAD rolled back — C6 `finalize_with_rollback` restores trunk when
    //    the terminal append fails.
    assert_eq!(
        ctx.head(),
        head_before,
        "C6 rollback should have restored HEAD after failed terminal append"
    );

    let all_events = events.events();

    // 3. THE INVARIANT UNDER TEST: fence event is durably recorded.
    let fence_count = all_events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ChangesetMaterializationStarted { .. }))
        .count();
    assert_eq!(
        fence_count, 1,
        "fence event must remain in the log — recovery needs it to detect \
         the orphan even though HEAD was rolled back"
    );

    // 4. The terminal must NOT be in the log (it's the one that faulted).
    let materialized_count = all_events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ChangesetMaterialized { .. }))
        .count();
    assert_eq!(materialized_count, 0);

    // 5. The fence landed BEFORE the fault attempt — the ordering C6
    //    depends on.
    let fence_idx = all_events
        .iter()
        .position(|e| matches!(e.kind, EventKind::ChangesetMaterializationStarted { .. }))
        .unwrap();
    let task_created_idx = all_events
        .iter()
        .position(|e| matches!(e.kind, EventKind::TaskCreated { .. }))
        .unwrap();
    assert!(
        task_created_idx < fence_idx,
        "fence must come after TaskCreated (causal order)"
    );
}
