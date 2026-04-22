//! End-to-end test for the active notification pipeline.
//!
//! Covers two guarantees that together make "push" delivery work:
//!
//! 1. After `submit_and_materialize`, the dep-only-affected agent has a
//!    single JSON file in `.phantom/overlays/<agent>/pending-notifications/`
//!    whose payload carries the `DependencyImpact` set and a rendered
//!    markdown summary.
//! 2. The dep-only ripple path no longer misclassifies the agent's own
//!    untouched-by-trunk files as `RebaseConflict`. The pending
//!    notification's `files` list must therefore be empty (the agent's
//!    interest in the trunk change is purely semantic, not filesystem
//!    overlap), while `dependency_impacts` is non-empty.
//!
//! Also exercises the simulated hook-drain loop by calling
//! `pending_notifications::mark_consumed` on each entry and asserting that
//! the queue is drained into `consumed/` — the same sequence the real
//! `phantom _notify-hook` subcommand performs on every Claude hook tick.

use std::path::PathBuf;

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::notification::TrunkFileStatus;
use phantom_core::traits::EventStore;
use phantom_orchestrator::materialization_service::ActiveOverlay;
use phantom_orchestrator::pending_notifications;
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
async fn ripple_queues_pending_notification_and_hook_drain_is_exactly_once() {
    let ctx = TestContext::new_async().await;

    let base = ctx.commit_files(&[
        ("src/auth.rs", TRUNK_LOGIN_V1),
        ("src/handlers.rs", CALLER_FILE),
    ]);

    // agent-a: changes the auth signature.
    let (agent_a, upper_a) =
        ctx.create_agent("agent-a", &[("src/auth.rs", TRUNK_LOGIN_V2_SIG_CHANGED)]);
    // agent-b: holds the caller file unchanged in its upper — a classic
    // dep-only ripple (trunk never changes handlers.rs, but agent-b
    // semantically depends on `login`).
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
    for agent_name in ["agent-a", "agent-b"] {
        std::fs::create_dir_all(phantom_dir.path().join("overlays").join(agent_name)).unwrap();
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
    .expect("submit_and_materialize errored")
    .expect("expected Some for non-empty overlay");

    // ----- Guarantee 1: a pending notification was queued for agent-b.
    let queued = pending_notifications::list(phantom_dir.path(), &agent_b).unwrap();
    assert_eq!(
        queued.len(),
        1,
        "expected exactly one pending notification for agent-b, got {queued:?}"
    );
    let payload = pending_notifications::load(&queued[0]).unwrap();
    assert_eq!(payload.submitting_agent, agent_a);
    assert!(
        !payload.notification.dependency_impacts.is_empty(),
        "pending notification must carry at least one dependency impact; got {:?}",
        payload.notification
    );
    assert!(
        payload
            .notification
            .dependency_impacts
            .iter()
            .any(|i| i.depends_on.name() == "login"),
        "expected `login` in dependency impacts: {:?}",
        payload.notification.dependency_impacts
    );
    assert!(
        payload.summary_md.contains("## Impacted Dependencies"),
        "summary markdown must contain the Impacted Dependencies section; got:\n{}",
        payload.summary_md
    );

    // ----- Guarantee 2: dep-only ripple does NOT mislabel agent-b's own
    // untouched-by-trunk files as Shadowed/RebaseConflict.
    for (path, status) in &payload.notification.files {
        assert!(
            !matches!(status, TrunkFileStatus::RebaseConflict),
            "dep-only ripple must not produce a RebaseConflict on {path:?} — trunk never changed it"
        );
    }
    assert!(
        payload.notification.files.is_empty(),
        "expected empty `files` on a dep-only ripple (agent-b's touched files did not change on trunk); got {:?}",
        payload.notification.files,
    );

    // ----- Simulate the hook drain sequence.
    // This is exactly what `phantom _notify-hook` does on every invocation:
    // list → load → render → mark_consumed.
    for path in &queued {
        pending_notifications::mark_consumed(path).unwrap();
    }
    let after_drain = pending_notifications::list(phantom_dir.path(), &agent_b).unwrap();
    assert!(
        after_drain.is_empty(),
        "queue must be empty after hook drain; leftover: {after_drain:?}"
    );

    let consumed = pending_notifications::consumed_dir(phantom_dir.path(), &agent_b);
    assert!(
        consumed.join("cs-a.json").exists()
            || std::fs::read_dir(&consumed).unwrap().any(|e| e
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "json")),
        "drained file must land in consumed/ (audit trail)"
    );

    // ----- A second drain (Claude fires UserPromptSubmit twice in a row,
    // e.g. user types back-to-back) must be a silent no-op. This is what
    // keeps notifications "exactly once".
    let second = pending_notifications::list(phantom_dir.path(), &agent_b).unwrap();
    assert!(second.is_empty());
}

#[tokio::test]
async fn file_overlap_ripple_marks_shadowed_and_queues_pending_notification() {
    // When the ripple is file-overlap (not dep-only), pending notification
    // must still be queued, and the `files` list must classify the overlap
    // as Shadowed / RebaseMerged / RebaseConflict (not left empty).
    let ctx = TestContext::new_async().await;

    const SHARED_V1: &str = "pub fn hello() { println!(\"hi\"); }\n";
    const SHARED_V2_TRUNK: &str = "pub fn hello() { println!(\"trunk says hi\"); }\n";
    const SHARED_AGENT_EDIT: &str = "pub fn hello() { println!(\"agent says hi\"); }\n";

    let base = ctx.commit_files(&[("src/shared.rs", SHARED_V1)]);

    let (agent_a, upper_a) = ctx.create_agent("agent-a", &[("src/shared.rs", SHARED_V2_TRUNK)]);
    let (agent_b, upper_b) = ctx.create_agent("agent-b", &[("src/shared.rs", SHARED_AGENT_EDIT)]);

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
    for agent_name in ["agent-a", "agent-b"] {
        std::fs::create_dir_all(phantom_dir.path().join("overlays").join(agent_name)).unwrap();
    }

    let active = vec![
        ActiveOverlay {
            agent_id: agent_a.clone(),
            files_touched: vec![PathBuf::from("src/shared.rs")],
            upper_dir: upper_a.path().to_path_buf(),
        },
        ActiveOverlay {
            agent_id: agent_b.clone(),
            files_touched: vec![PathBuf::from("src/shared.rs")],
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
    .unwrap()
    .unwrap();

    let queued = pending_notifications::list(phantom_dir.path(), &agent_b).unwrap();
    assert_eq!(queued.len(), 1, "expected one pending notification queued");
    let payload = pending_notifications::load(&queued[0]).unwrap();
    assert!(
        payload
            .notification
            .files
            .iter()
            .any(|(p, status)| p.to_str() == Some("src/shared.rs")
                && matches!(
                    status,
                    TrunkFileStatus::Shadowed
                        | TrunkFileStatus::RebaseMerged
                        | TrunkFileStatus::RebaseConflict
                )),
        "overlap ripple must classify src/shared.rs with a shadow-related status; got {:?}",
        payload.notification.files
    );
}

#[tokio::test]
async fn no_impact_ripple_does_not_queue_pending_notification() {
    // Agent-b has no overlap and no dependency on the changed symbol —
    // no pending notification should be enqueued (don't spam the prompt
    // cache with empty updates).
    let ctx = TestContext::new_async().await;

    const TRUNK_A: &str = "pub fn a() -> u32 { 1 }\n";
    const TRUNK_A_V2: &str = "pub fn a() -> u32 { 2 }\n";
    // agent-b's file has no reference to `a` — pure no-op ripple.
    const INDEPENDENT: &str = "pub fn z() -> u32 { 99 }\n";

    let base = ctx.commit_files(&[("src/a.rs", TRUNK_A), ("src/b.rs", INDEPENDENT)]);

    let (agent_a, upper_a) = ctx.create_agent("agent-a", &[("src/a.rs", TRUNK_A_V2)]);
    let (agent_b, upper_b) = ctx.create_agent("agent-b", &[("src/b.rs", INDEPENDENT)]);

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
    for agent_name in ["agent-a", "agent-b"] {
        std::fs::create_dir_all(phantom_dir.path().join("overlays").join(agent_name)).unwrap();
    }

    let active = vec![
        ActiveOverlay {
            agent_id: agent_a.clone(),
            files_touched: vec![PathBuf::from("src/a.rs")],
            upper_dir: upper_a.path().to_path_buf(),
        },
        ActiveOverlay {
            agent_id: agent_b.clone(),
            files_touched: vec![PathBuf::from("src/b.rs")],
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
    .unwrap()
    .unwrap();

    let queued = pending_notifications::list(phantom_dir.path(), &agent_b).unwrap();
    assert!(
        queued.is_empty(),
        "no-impact ripple must not enqueue a pending notification; got {queued:?}"
    );
}

#[tokio::test]
async fn pending_notification_filename_matches_changeset_id() {
    // Regression: the filename is used for idempotency. The test above
    // asserts "cs-a.json" — this one locks in that contract independently
    // so a future refactor cannot silently change it.
    use phantom_orchestrator::pending_notifications::{PendingNotification, write};

    let tmp = tempfile::tempdir().unwrap();
    let phantom_dir = tmp.path();
    let agent_id = AgentId("agent-b".into());
    std::fs::create_dir_all(phantom_dir.join("overlays/agent-b")).unwrap();

    let payload = PendingNotification {
        changeset_id: ChangesetId("cs-42".into()),
        submitting_agent: AgentId("agent-a".into()),
        notification: phantom_core::notification::TrunkNotification {
            new_commit: phantom_core::id::GitOid::zero(),
            timestamp: chrono::Utc::now(),
            files: vec![],
            dependency_impacts: vec![],
        },
        summary_md: "# Trunk Update\n".into(),
    };
    write(phantom_dir, &agent_id, &payload).unwrap();

    assert!(
        phantom_dir
            .join("overlays/agent-b/pending-notifications/cs-42.json")
            .exists(),
        "filename contract: {{changeset_id}}.json"
    );
}
