//! Integration test for the pre-submit outbound warning.
//!
//! When an agent submits a signature change or deletion and another active
//! agent references the affected symbol, a warning is appended to the
//! submitter's `.phantom-task.md` BEFORE materialization runs.

use std::path::PathBuf;

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::materialization_service::ActiveOverlay;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayLayer;
use phantom_testkit::TestContext;
use phantom_testkit::mocks::MockEventStore;

const AUTH_V1: &str = r#"pub mod auth {
    pub fn login(user_id: u32) -> bool {
        user_id > 0
    }
}
"#;

const AUTH_V2: &str = r#"pub mod auth {
    pub fn login(user_id: u32, token: &str) -> bool {
        user_id > 0 && !token.is_empty()
    }
}
"#;

const CALLER: &str = r#"pub mod handlers {
    pub fn handle_request() -> bool {
        crate::auth::login(42)
    }
}
"#;

#[tokio::test]
async fn pre_submit_warning_appears_in_submitter_context_file() {
    let ctx = TestContext::new_async().await;
    let base =
        ctx.commit_files(&[("src/auth.rs", AUTH_V1), ("src/handlers.rs", CALLER)]);

    // Agent-a is about to make a breaking signature change.
    let (agent_a, upper_a) = ctx.create_agent("agent-a", &[("src/auth.rs", AUTH_V2)]);

    // Seed agent-a's context file so the warning has a target to append into.
    std::fs::write(
        upper_a.path().join(".phantom-task.md"),
        "# Agent\n\n---\n\n## Trunk Updates\n",
    )
    .unwrap();

    // Agent-b is active and calls the function.
    let (agent_b, upper_b) = ctx.create_agent("agent-b", &[("src/handlers.rs", CALLER)]);

    let events = MockEventStore::new();
    for (cs_id, agent) in [("cs-a", &agent_a), ("cs-b", &agent_b)] {
        events
            .append(Event {
                id: EventId(0),
                timestamp: chrono::Utc::now(),
                changeset_id: ChangesetId(cs_id.into()),
                agent_id: agent.clone(),
                causal_parent: None,
                kind: EventKind::TaskCreated {
                    base_commit: base,
                    task: "test".into(),
                },
            })
            .await
            .unwrap();
    }

    let trunk_path = ctx.git.repo().workdir().unwrap().to_path_buf();
    let layer_a = OverlayLayer::new(trunk_path.clone(), upper_a.path().to_path_buf()).unwrap();
    let phantom_dir = tempfile::TempDir::new().unwrap();
    for name in ["agent-a", "agent-b"] {
        std::fs::create_dir_all(phantom_dir.path().join("overlays").join(name)).unwrap();
    }

    let active = vec![
        ActiveOverlay {
            agent_id: agent_a.clone(),
            files_touched: vec![PathBuf::from("src/auth.rs")],
            upper_dir: upper_a.path().to_path_buf(),
        },
        ActiveOverlay {
            agent_id: agent_b,
            files_touched: vec![PathBuf::from("src/handlers.rs")],
            upper_dir: upper_b.path().to_path_buf(),
        },
    ];

    submit_service::submit_and_materialize(
        &ctx.git,
        &events,
        &ctx.merger,
        &agent_a,
        &layer_a,
        upper_a.path(),
        phantom_dir.path(),
        &ctx.materializer(),
        &active,
        None,
    )
    .await
    .expect("submit_and_materialize failed");

    let task_md = std::fs::read_to_string(upper_a.path().join(".phantom-task.md")).unwrap();
    assert!(
        task_md.contains("Pre-Submit Dependency Warning"),
        "expected pre-submit warning in .phantom-task.md:\n{task_md}"
    );
    assert!(
        task_md.contains("agent-b"),
        "warning must name the affected agent"
    );
    assert!(
        task_md.contains("signature changed"),
        "warning must describe the signature change"
    );
}
