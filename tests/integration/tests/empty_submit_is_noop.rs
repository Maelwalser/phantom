//! Integration test: submitting an agent whose overlay has no changes is a
//! no-op — no git commit, no events, no panics.
//!
//! Pins the contract of `submit_and_materialize` that an agent with an
//! empty upper layer returns `Ok(None)`.

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayLayer;
use phantom_testkit::TestContext;
use phantom_testkit::mocks::MockEventStore;

#[tokio::test]
async fn empty_overlay_submit_returns_none_and_leaves_trunk_unchanged() {
    let ctx = TestContext::new_async().await;

    // Seed trunk.
    let base = ctx.commit_files(&[("src/lib.rs", "fn a() {}\n")]);

    // Create an agent overlay with no modifications — the upper is empty.
    let (agent_id, upper) = ctx.create_agent("agent-idle", &[]);

    // Record the TaskCreated event so discovery succeeds.
    let events = MockEventStore::new();
    events
        .append(Event {
            id: EventId(0),
            timestamp: chrono::Utc::now(),
            changeset_id: ChangesetId("cs-idle".into()),
            agent_id: agent_id.clone(),
            causal_parent: None,
            kind: EventKind::TaskCreated {
                base_commit: base,
                task: "do nothing".into(),
            },
        })
        .await
        .unwrap();

    let trunk_path = ctx.git.repo().workdir().unwrap().to_path_buf();
    let layer = OverlayLayer::new(trunk_path, upper.path().to_path_buf()).unwrap();
    let phantom_dir = tempfile::TempDir::new().unwrap();

    let head_before = ctx.head();

    let output = submit_service::submit_and_materialize(
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
    .await
    .expect("empty submit must not error");

    assert!(output.is_none(), "empty overlay must submit as a no-op");
    assert_eq!(ctx.head(), head_before, "trunk must not advance on no-op");

    // No ChangesetSubmitted event should have been written either.
    let submitted = events
        .events()
        .into_iter()
        .filter(|e| matches!(e.kind, EventKind::ChangesetSubmitted { .. }))
        .count();
    assert_eq!(submitted, 0);
}
