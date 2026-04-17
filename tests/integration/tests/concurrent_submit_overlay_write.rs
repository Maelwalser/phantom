//! Integration test: running `submit_and_materialize` while an unrelated
//! writer is churning on the upper directory must never panic and must
//! produce a consistent outcome (either a valid submission or a clean
//! error).
//!
//! This pins the robustness of the overlay scan + semantic extraction
//! against concurrent mutation.  The background writer does not modify
//! the files being submitted — it writes an unrelated file in a tight
//! loop — so the expected outcome is a clean success.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayLayer;
use phantom_testkit::TestContext;
use phantom_testkit::mocks::MockEventStore;

#[tokio::test]
async fn concurrent_unrelated_writer_does_not_destabilize_submit() {
    let ctx = TestContext::new_async().await;

    let base = ctx.commit_files(&[("src/lib.rs", "fn a() {}\n")]);

    let (agent_id, upper) =
        ctx.create_agent("agent-busy", &[("src/lib.rs", "fn a() {}\nfn b() {}\n")]);

    let events = MockEventStore::new();
    events
        .append(Event {
            id: EventId(0),
            timestamp: chrono::Utc::now(),
            changeset_id: ChangesetId("cs-busy".into()),
            agent_id: agent_id.clone(),
            causal_parent: None,
            kind: EventKind::TaskCreated {
                base_commit: base,
                task: "add fn b".into(),
            },
        })
        .await
        .unwrap();

    // Background writer scratches an unrelated file in the upper dir.
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let upper_path = upper.path().to_path_buf();
    let writer = thread::spawn(move || {
        let scratch = upper_path.join("scratch.log");
        let mut counter = 0u64;
        while !stop_clone.load(Ordering::Relaxed) {
            let _ = std::fs::write(&scratch, format!("tick {counter}\n"));
            counter = counter.wrapping_add(1);
            thread::sleep(Duration::from_micros(50));
        }
    });

    // Let the writer spin for a bit so it overlaps with submit.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let trunk_path = ctx.git.repo().workdir().unwrap().to_path_buf();
    let layer = OverlayLayer::new(trunk_path, upper.path().to_path_buf()).unwrap();
    let phantom_dir = tempfile::TempDir::new().unwrap();

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

    // Stop the writer regardless of outcome.
    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer thread panicked");

    let output = result
        .expect("submit must not error due to concurrent unrelated writes")
        .expect("submit must detect the src/lib.rs modification");

    assert!(
        output
            .submit
            .modified_files
            .iter()
            .any(|p| p == &PathBuf::from("src/lib.rs")),
        "expected src/lib.rs in submission, got {:?}",
        output.submit.modified_files
    );

    // Trunk must contain fn b.
    let head_content = ctx.read_file_at_head("src/lib.rs");
    assert!(
        head_content.contains("fn b"),
        "trunk must reflect the agent's modification: {head_content}"
    );
}
