//! End-to-end integration tests that exercise the semantic dependency graph
//! against TypeScript, Python, and Go sources.
//!
//! These tests confirm that each language's reference extractor produces
//! usable edges that flow all the way through to [`DependencyImpact`]s in
//! the ripple notification — not just unit-level extraction.

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

async fn run_ripple_scenario(
    changed_path: &str,
    caller_path: &str,
    base_contents: &[(&str, &str)],
    new_auth_content: &str,
    caller_content: &str,
) -> TrunkNotification {
    let ctx = TestContext::new_async().await;
    let base = ctx.commit_files(base_contents);

    let (agent_a, upper_a) = ctx.create_agent("agent-a", &[(changed_path, new_auth_content)]);
    let (agent_b, upper_b) = ctx.create_agent("agent-b", &[(caller_path, caller_content)]);

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
            files_touched: vec![PathBuf::from(changed_path)],
            upper_dir: upper_a.path().to_path_buf(),
        },
        ActiveOverlay {
            agent_id: agent_b.clone(),
            files_touched: vec![PathBuf::from(caller_path)],
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

    let notif_path = phantom_dir
        .path()
        .join("overlays")
        .join("agent-b")
        .join("trunk-updated.json");
    let raw = std::fs::read_to_string(&notif_path).expect("trunk-updated.json must exist");
    serde_json::from_str(&raw).expect("valid trunk notification")
}

#[tokio::test]
async fn typescript_signature_change_produces_impact() {
    const BASE: &str = r#"export function login(userId: number): boolean {
    return userId > 0;
}
"#;
    const NEW: &str = r#"export function login(userId: number, token: string): boolean {
    return userId > 0 && token.length > 0;
}
"#;
    const CALLER: &str = r#"import { login } from './auth';

export function handleRequest(): boolean {
    return login(42);
}
"#;

    let notif = run_ripple_scenario(
        "src/auth.ts",
        "src/handlers.ts",
        &[("src/auth.ts", BASE), ("src/handlers.ts", CALLER)],
        NEW,
        CALLER,
    )
    .await;

    let login_impact = notif
        .dependency_impacts
        .iter()
        .find(|i| i.depends_on.name() == "login")
        .unwrap_or_else(|| {
            panic!(
                "expected an impact naming login, got: {:?}",
                notif.dependency_impacts
            )
        });
    assert_eq!(login_impact.change, ImpactChange::SignatureChanged);
    assert_eq!(login_impact.your_symbol.name(), "handleRequest");
}

#[tokio::test]
async fn python_signature_change_produces_impact() {
    const BASE: &str = r#"def login(user_id):
    return user_id > 0
"#;
    const NEW: &str = r#"def login(user_id, token):
    return user_id > 0 and len(token) > 0
"#;
    const CALLER: &str = r#"from auth import login

def handle_request():
    return login(42)
"#;

    let notif = run_ripple_scenario(
        "src/auth.py",
        "src/handlers.py",
        &[("src/auth.py", BASE), ("src/handlers.py", CALLER)],
        NEW,
        CALLER,
    )
    .await;

    let login_impact = notif
        .dependency_impacts
        .iter()
        .find(|i| i.depends_on.name() == "login")
        .unwrap_or_else(|| {
            panic!(
                "expected python login impact, got: {:?}",
                notif.dependency_impacts
            )
        });
    assert_eq!(login_impact.change, ImpactChange::SignatureChanged);
    assert_eq!(login_impact.your_symbol.name(), "handle_request");
}

#[tokio::test]
async fn go_signature_change_produces_impact() {
    const BASE: &str = r#"package auth

func Login(userID uint32) bool {
    return userID > 0
}
"#;
    const NEW: &str = r#"package auth

func Login(userID uint32, token string) bool {
    return userID > 0 && len(token) > 0
}
"#;
    const CALLER: &str = r#"package handlers

import "example/auth"

func HandleRequest() bool {
    return auth.Login(42)
}
"#;

    let notif = run_ripple_scenario(
        "src/auth.go",
        "src/handlers.go",
        &[("src/auth.go", BASE), ("src/handlers.go", CALLER)],
        NEW,
        CALLER,
    )
    .await;

    let login_impact = notif
        .dependency_impacts
        .iter()
        .find(|i| i.depends_on.name() == "Login")
        .unwrap_or_else(|| {
            panic!(
                "expected go Login impact, got: {:?}",
                notif.dependency_impacts
            )
        });
    assert_eq!(login_impact.change, ImpactChange::SignatureChanged);
    assert_eq!(login_impact.your_symbol.name(), "HandleRequest");
}

#[tokio::test]
async fn trunk_preview_contains_signature_diff_snippet() {
    const BASE: &str = r#"pub mod auth {
    pub fn login(user_id: u32) -> bool {
        user_id > 0
    }
}
"#;
    const NEW: &str = r#"pub mod auth {
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

    let notif = run_ripple_scenario(
        "src/auth.rs",
        "src/handlers.rs",
        &[("src/auth.rs", BASE), ("src/handlers.rs", CALLER)],
        NEW,
        CALLER,
    )
    .await;

    let login = notif
        .dependency_impacts
        .iter()
        .find(|i| i.depends_on.name() == "login")
        .expect("expected login impact");
    let preview = login
        .trunk_preview
        .as_ref()
        .expect("trunk_preview should be enriched");
    assert!(
        preview.contains("token"),
        "preview should mention the added `token` param, got {preview}"
    );
}
