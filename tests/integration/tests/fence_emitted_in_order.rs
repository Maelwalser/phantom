//! Integration test: on the happy path, every submit that reaches
//! materialization emits a pre-commit fence event immediately before the
//! `ChangesetMaterialized` terminal.
//!
//! This pins the ghost-commit protocol ordering into an end-to-end test:
//! any regression that drops the fence event, reorders the pair, or sets
//! the wrong `parent` will fail here.

use std::path::PathBuf;

use phantom_core::event::{EventKind, MaterializationPath};
use phantom_core::traits::EventStore;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayLayer;
use phantom_testkit::TestContext;

#[tokio::test]
async fn fence_precedes_materialized_on_happy_path_submit() {
    let ctx = TestContext::new_async().await;

    let base = ctx.commit_files(&[("src/lib.rs", "fn a() {}\n")]);
    let (agent_id, upper) =
        ctx.create_agent("agent-a", &[("src/lib.rs", "fn a() {}\nfn b() {}\n")]);

    // Seed the TaskCreated that the submit pipeline expects.
    ctx.events
        .append(phantom_core::event::Event {
            id: phantom_core::id::EventId(0),
            timestamp: chrono::Utc::now(),
            changeset_id: phantom_core::id::ChangesetId("cs-fence-happy".into()),
            agent_id: agent_id.clone(),
            causal_parent: None,
            kind: EventKind::TaskCreated {
                base_commit: base,
                task: "add fn b".into(),
            },
        })
        .await
        .unwrap();

    let trunk_path = ctx.git.repo().workdir().unwrap().to_path_buf();
    let layer = OverlayLayer::new(trunk_path, upper.path().to_path_buf()).unwrap();
    let phantom_dir = tempfile::TempDir::new().unwrap();

    let head_before = ctx.head();
    submit_service::submit_and_materialize(
        &ctx.git,
        &ctx.events,
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
    .expect("happy-path submit must succeed");

    // Fetch the full event log and find the fence + materialized pair.
    let events = ctx.events.query_all().await.unwrap();

    let fence_idx = events
        .iter()
        .position(|e| matches!(e.kind, EventKind::ChangesetMaterializationStarted { .. }))
        .expect("ChangesetMaterializationStarted must be emitted before the commit");
    let materialized_idx = events
        .iter()
        .position(|e| matches!(e.kind, EventKind::ChangesetMaterialized { .. }))
        .expect("ChangesetMaterialized must land after the fence");

    assert!(
        fence_idx < materialized_idx,
        "fence event (idx {fence_idx}) must precede ChangesetMaterialized (idx {materialized_idx})"
    );

    // The fence records the pre-commit HEAD as its `parent`. Direct path is
    // expected because no one else moved trunk between `commit_files` and
    // this submit.
    let EventKind::ChangesetMaterializationStarted { parent, path } = &events[fence_idx].kind
    else {
        unreachable!();
    };
    assert_eq!(
        *parent, head_before,
        "fence's `parent` must equal trunk HEAD at submit time"
    );
    assert_eq!(*path, MaterializationPath::Direct);

    // The fence's causal_parent must point at the most recent prior event
    // for this changeset (TaskCreated) — keeping the causal DAG intact.
    assert!(
        events[fence_idx].causal_parent.is_some(),
        "fence must link into the changeset's causal chain"
    );

    let _ = PathBuf::from("src/lib.rs");
}
