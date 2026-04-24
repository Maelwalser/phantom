//! Integration test: simulate a crash in the narrow window between
//! materialization (git commit) and the final `ChangesetSubmitted` event
//! append.
//!
//! This exercises the deliberate H-ORC2 ordering in the submit pipeline:
//! if the event store fails after trunk has moved, the trunk commit is
//! durable but the audit-trail event is missing. The test pins that
//! behavior so the invariant ("trunk moves, no orphan event") remains
//! asserted by CI.

use std::path::PathBuf;

use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::error::OrchestratorError;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayLayer;
use phantom_testkit::TestContext;
use phantom_testkit::mocks::MockEventStore;

#[tokio::test]
async fn changeset_submitted_failure_leaves_trunk_committed_and_no_event() {
    let ctx = TestContext::new_async().await;

    // Seed trunk.
    let base = ctx.commit_files(&[("src/lib.rs", "fn a() {}\n")]);

    // Create an agent overlay that adds a new function.
    let (agent_id, upper) =
        ctx.create_agent("agent-a", &[("src/lib.rs", "fn a() {}\nfn b() {}\n")]);

    // Event store that rejects the final ChangesetSubmitted write, mimicking
    // a crash between git commit and audit-event persistence.
    let events = MockEventStore::new();
    events
        .append(Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: ChangesetId("cs-crash".into()),
            agent_id: agent_id.clone(),
            causal_parent: None,
            kind: EventKind::TaskCreated {
                base_commit: base,
                task: "add fn b".into(),
            },
        })
        .await
        .expect("seeding TaskCreated must succeed");

    events.fail_when(|k| matches!(k, EventKind::ChangesetSubmitted { .. }));

    // Overlay layer required by submit_and_materialize.
    let trunk_path = ctx.git.repo().workdir().unwrap().to_path_buf();
    let layer = OverlayLayer::new(trunk_path, upper.path().to_path_buf())
        .expect("failed to create overlay layer");

    // phantom_dir only needs to exist; nothing in this test reads from it
    // until after materialization, which will not be reached when the
    // ripple step emits no-op notifications.
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

    // 1. The pipeline returns an EventStore error (the fault).
    assert!(
        matches!(result, Err(OrchestratorError::EventStore(_))),
        "expected EventStore error, got: {result:?}"
    );

    // 2. Trunk must have advanced — materialize ran successfully before
    //    the event append. This is the point of H-ORC2.
    let head_after = ctx.head();
    assert_ne!(
        head_before, head_after,
        "materialization must commit to trunk before the event append is attempted"
    );

    // 3. The event log must NOT contain a `ChangesetSubmitted` event —
    //    that is the entry whose write was faulted.
    let log = events.events();
    let submitted = log
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ChangesetSubmitted { .. }))
        .count();
    assert_eq!(
        submitted, 0,
        "no ChangesetSubmitted event should have been persisted"
    );

    // 3b. Fence + materialized are both durable: the crash happened *after*
    //     materialization succeeded, so both the pre-commit fence and the
    //     materialized terminal must be in the log. This is what recovery
    //     uses to determine the commit is legitimate audit-wise even though
    //     `ChangesetSubmitted` is missing.
    let fence = log
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ChangesetMaterializationStarted { .. }))
        .count();
    let materialized = log
        .iter()
        .filter(|e| matches!(e.kind, EventKind::ChangesetMaterialized { .. }))
        .count();
    assert_eq!(fence, 1, "fence event must be durable after H-ORC2 fault");
    assert_eq!(
        materialized, 1,
        "materialized terminal must be durable after H-ORC2 fault"
    );

    // 4. The trunk-advance commit still contains the agent's change,
    //    so downstream recovery can rebuild the missing audit event
    //    from git metadata if needed.
    let content = ctx.read_file_at_head("src/lib.rs");
    assert!(
        content.contains("fn b"),
        "trunk must contain the agent's change despite the event fault"
    );

    // Keep symbols used so the test imports don't go stale.
    let _ = PathBuf::from("src/lib.rs");
}
