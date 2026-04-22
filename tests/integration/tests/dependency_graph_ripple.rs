//! End-to-end integration test for the semantic dependency graph.
//!
//! Scenario:
//! * Trunk defines `crate::auth::login(u32) -> bool`.
//! * Agent-a changes `login`'s signature: `login(u32, &str) -> bool`.
//! * Agent-b has an upper-layer file that calls `login` from inside its own
//!   `crate::handlers::handle_request` function.
//! * Agent-a submits through the full `submit_and_materialize` pipeline.
//!
//! After materialization, agent-b should receive a `trunk-updated.json`
//! whose `dependency_impacts` field names the call from `handle_request` →
//! `login` with `ImpactChange::SignatureChanged`, and the rendered
//! `.phantom-trunk-update.md` should contain the "Impacted Dependencies"
//! section.

use std::path::PathBuf;

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{ChangesetId, EventId};
use phantom_core::notification::{ImpactChange, TrunkNotification};
use phantom_core::traits::EventStore;
use phantom_orchestrator::materialization_service::ActiveOverlay;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayLayer;
use phantom_testkit::TestContext;
use phantom_testkit::mocks::MockEventStore;

const TRUNK_LOGIN_V1: &str = r#"pub mod auth {
    pub fn login(user_id: u32) -> bool {
        user_id > 0
    }
}
"#;

const TRUNK_LOGIN_V2_SIG_CHANGED: &str = r#"pub mod auth {
    pub fn login(user_id: u32, token: &str) -> bool {
        user_id > 0 && !token.is_empty()
    }
}
"#;

const CALLER_FILE: &str = r#"pub mod handlers {
    pub fn handle_request() -> bool {
        crate::auth::login(42)
    }
}
"#;

#[tokio::test]
async fn ripple_emits_signature_changed_impact_for_dependent_agent() {
    let ctx = TestContext::new_async().await;

    // Seed trunk: both files exist at v1.
    let base = ctx.commit_files(&[
        ("src/auth.rs", TRUNK_LOGIN_V1),
        ("src/handlers.rs", CALLER_FILE),
    ]);

    // Agent-a: changes login signature in src/auth.rs. Upper layer contains
    // only the modified file.
    let (agent_a, upper_a) =
        ctx.create_agent("agent-a", &[("src/auth.rs", TRUNK_LOGIN_V2_SIG_CHANGED)]);

    // Agent-b: has src/handlers.rs in its upper layer (it's already "working"
    // on this file, even if unchanged).
    let (agent_b, upper_b) = ctx.create_agent("agent-b", &[("src/handlers.rs", CALLER_FILE)]);

    // Both agents need a TaskCreated event so discovery succeeds.
    let events = MockEventStore::new();
    for (cs_id, agent, _) in [("cs-a", &agent_a, &upper_a), ("cs-b", &agent_b, &upper_b)] {
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

    // `.phantom/overlays/<agent>/` is where ripple writes notifications.
    let phantom_dir = tempfile::TempDir::new().unwrap();
    for agent_name in ["agent-a", "agent-b"] {
        std::fs::create_dir_all(phantom_dir.path().join("overlays").join(agent_name)).unwrap();
    }

    // Active overlays — agent-b should receive the ripple.
    let active = vec![
        ActiveOverlay {
            agent_id: agent_a.clone(),
            files_touched: vec![PathBuf::from("src/auth.rs")],
            upper_dir: upper_a.path().to_path_buf(),
        },
        ActiveOverlay {
            agent_id: agent_b.clone(),
            files_touched: vec![PathBuf::from("src/handlers.rs")],
            upper_dir: upper_b.path().to_path_buf(),
        },
    ];

    // Submit agent-a through the full pipeline.
    let output = submit_service::submit_and_materialize(
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
    .expect("submit_and_materialize failed")
    .expect("expected Some output for non-empty overlay");

    assert!(
        matches!(
            output.materialize.result,
            phantom_orchestrator::materializer::MaterializeResult::Success { .. }
        ),
        "expected clean materialization"
    );

    // Read agent-b's trunk-updated.json and verify dependency_impacts.
    let notif_path = phantom_dir
        .path()
        .join("overlays")
        .join("agent-b")
        .join("trunk-updated.json");
    assert!(
        notif_path.exists(),
        "agent-b must have received trunk-updated.json, looked for {notif_path:?}"
    );
    let raw = std::fs::read_to_string(&notif_path).unwrap();
    let notif: TrunkNotification =
        serde_json::from_str(&raw).expect("trunk-updated.json must be valid JSON");

    assert!(
        !notif.dependency_impacts.is_empty(),
        "expected at least one dependency impact, got none. Raw: {raw}"
    );

    let login_impact = notif
        .dependency_impacts
        .iter()
        .find(|i| i.depends_on.name() == "login")
        .expect("expected an impact naming `login` as the dependency");

    assert_eq!(
        login_impact.change,
        ImpactChange::SignatureChanged,
        "signature change must be detected (old_signature_hash != new_signature_hash)"
    );
    assert_eq!(login_impact.your_symbol.name(), "handle_request");

    // The rendered markdown should contain the Impacted Dependencies section.
    let md_path = upper_b.path().join(".phantom-trunk-update.md");
    if md_path.exists() {
        let md = std::fs::read_to_string(&md_path).unwrap();
        assert!(
            md.contains("## Impacted Dependencies"),
            "expected `## Impacted Dependencies` section in {md}"
        );
        assert!(md.contains("signature changed"));
        assert!(md.contains("login"));
    }

    // AgentNotified event should have been recorded.
    let notified = events
        .events()
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::AgentNotified {
                agent_id,
                changed_symbols,
            } if agent_id == agent_b => Some(changed_symbols),
            _ => None,
        })
        .expect("expected AgentNotified event for agent-b");
    assert!(
        notified.iter().any(|s| s.name() == "login"),
        "AgentNotified.changed_symbols must include `login`"
    );
}

#[tokio::test]
async fn body_only_change_does_not_emit_signature_changed_impact() {
    const V1: &str = r#"pub mod auth {
    pub fn login(user_id: u32) -> bool {
        user_id > 0
    }
}
"#;
    // Same signature, different body.
    const V2: &str = r#"pub mod auth {
    pub fn login(user_id: u32) -> bool {
        user_id >= 1
    }
}
"#;

    let ctx = TestContext::new_async().await;
    let base = ctx.commit_files(&[("src/auth.rs", V1), ("src/handlers.rs", CALLER_FILE)]);

    let (agent_a, upper_a) = ctx.create_agent("agent-a", &[("src/auth.rs", V2)]);
    let (agent_b, upper_b) = ctx.create_agent("agent-b", &[("src/handlers.rs", CALLER_FILE)]);

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
            agent_id: agent_b.clone(),
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
    .expect("submit failed");

    let notif_path = phantom_dir
        .path()
        .join("overlays")
        .join("agent-b")
        .join("trunk-updated.json");
    let raw = std::fs::read_to_string(&notif_path).unwrap();
    let notif: TrunkNotification = serde_json::from_str(&raw).unwrap();

    // If any impact is emitted, it must NOT be SignatureChanged.
    for impact in &notif.dependency_impacts {
        assert_ne!(
            impact.change,
            ImpactChange::SignatureChanged,
            "body-only change must not be classified as SignatureChanged; got {impact:?}"
        );
    }
}
