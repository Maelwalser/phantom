//! Tests for the [`Projection`] state derivation engine.

use chrono::{self, Utc};
use phantom_core::changeset::ChangesetStatus;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};

use phantom_events::Projection;

fn make_event(
    id: u64,
    changeset: &str,
    agent: &str,
    kind: EventKind,
    timestamp: chrono::DateTime<Utc>,
) -> Event {
    Event {
        id: EventId(id),
        timestamp,
        changeset_id: ChangesetId(changeset.into()),
        agent_id: AgentId(agent.into()),
        causal_parent: None,
        kind,
    }
}

#[test]
fn latest_submitted_changeset_returns_none_when_no_changesets() {
    let projection = Projection::from_events(&[]);
    assert!(
        projection
            .latest_submitted_changeset(&AgentId("agent-a".into()))
            .is_none()
    );
}

#[test]
fn latest_submitted_changeset_returns_none_when_only_in_progress() {
    let t = Utc::now();
    let events = vec![make_event(
        1,
        "cs-0001",
        "agent-a",
        EventKind::TaskCreated {
            base_commit: GitOid::zero(),
            task: "task".into(),
        },
        t,
    )];
    let projection = Projection::from_events(&events);
    assert!(
        projection
            .latest_submitted_changeset(&AgentId("agent-a".into()))
            .is_none()
    );
}

#[test]
fn latest_submitted_changeset_returns_submitted_for_correct_agent() {
    let t = Utc::now();
    let events = vec![
        // Agent A: overlay created then submitted
        make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "task-a".into(),
            },
            t,
        ),
        make_event(
            2,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t,
        ),
        // Agent B: overlay created then submitted
        make_event(
            3,
            "cs-0002",
            "agent-b",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "task-b".into(),
            },
            t,
        ),
        make_event(
            4,
            "cs-0002",
            "agent-b",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t,
        ),
    ];
    let projection = Projection::from_events(&events);

    let result = projection.latest_submitted_changeset(&AgentId("agent-a".into()));
    assert!(result.is_some());
    assert_eq!(result.unwrap().id.0, "cs-0001");

    let result = projection.latest_submitted_changeset(&AgentId("agent-b".into()));
    assert!(result.is_some());
    assert_eq!(result.unwrap().id.0, "cs-0002");
}

#[test]
fn latest_submitted_changeset_picks_most_recent_when_multiple_submitted() {
    let t1 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let t2 = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let events = vec![
        // First changeset: created at t1, submitted
        make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "old task".into(),
            },
            t1,
        ),
        make_event(
            2,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t1,
        ),
        // Second changeset: created at t2, submitted (newer)
        make_event(
            3,
            "cs-0002",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "new task".into(),
            },
            t2,
        ),
        make_event(
            4,
            "cs-0002",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t2,
        ),
    ];
    let projection = Projection::from_events(&events);

    let result = projection
        .latest_submitted_changeset(&AgentId("agent-a".into()))
        .unwrap();
    assert_eq!(result.id.0, "cs-0002");
}

#[test]
fn latest_submitted_changeset_picks_most_recent_skipping_conflicted() {
    let t1 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let t2 = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let t3 = chrono::DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let events = vec![
        // cs-0001: submitted and materialized (stays Submitted)
        make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "done".into(),
            },
            t1,
        ),
        make_event(
            2,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t1,
        ),
        make_event(
            3,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetMaterialized {
                new_commit: GitOid::zero(),
            },
            t1,
        ),
        // cs-0002: submitted (still pending) — this is the one we want
        make_event(
            4,
            "cs-0002",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "pending".into(),
            },
            t2,
        ),
        make_event(
            5,
            "cs-0002",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t2,
        ),
        // cs-0003: submitted then conflicted
        make_event(
            6,
            "cs-0003",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "conflict".into(),
            },
            t3,
        ),
        make_event(
            7,
            "cs-0003",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t3,
        ),
        make_event(
            8,
            "cs-0003",
            "agent-a",
            EventKind::ChangesetConflicted { conflicts: vec![] },
            t3,
        ),
    ];
    let projection = Projection::from_events(&events);

    let result = projection
        .latest_submitted_changeset(&AgentId("agent-a".into()))
        .unwrap();
    assert_eq!(result.id.0, "cs-0002");
}

#[test]
fn conflict_resolution_updates_base_commit() {
    let t = Utc::now();
    let original_base = GitOid::zero();
    let new_base = GitOid::from_bytes([0xAA; 20]);

    let events = vec![
        make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: original_base,
                task: "task".into(),
            },
            t,
        ),
        make_event(
            2,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t,
        ),
        make_event(
            3,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetConflicted { conflicts: vec![] },
            t,
        ),
        // ConflictResolutionStarted updates base_commit to new_base
        make_event(
            4,
            "cs-0001",
            "agent-a",
            EventKind::ConflictResolutionStarted {
                conflicts: vec![],
                new_base: Some(new_base),
            },
            t,
        ),
    ];
    let projection = Projection::from_events(&events);

    let cs = projection
        .changeset(&ChangesetId("cs-0001".into()))
        .unwrap();
    assert_eq!(
        cs.base_commit, new_base,
        "base_commit should be updated by ConflictResolutionStarted"
    );
}

#[test]
fn conflict_resolution_without_new_base_preserves_original() {
    let t = Utc::now();
    let original_base = GitOid::from_bytes([0xBB; 20]);

    let events = vec![
        make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: original_base,
                task: "task".into(),
            },
            t,
        ),
        // Legacy event without new_base field
        make_event(
            2,
            "cs-0001",
            "agent-a",
            EventKind::ConflictResolutionStarted {
                conflicts: vec![],
                new_base: None,
            },
            t,
        ),
    ];
    let projection = Projection::from_events(&events);

    let cs = projection
        .changeset(&ChangesetId("cs-0001".into()))
        .unwrap();
    assert_eq!(
        cs.base_commit, original_base,
        "base_commit should be unchanged when new_base is None"
    );
}

#[test]
fn conflict_resolution_transitions_to_resolving() {
    let t = Utc::now();
    let events = vec![
        make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "task".into(),
            },
            t,
        ),
        make_event(
            2,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t,
        ),
        make_event(
            3,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetConflicted { conflicts: vec![] },
            t,
        ),
        make_event(
            4,
            "cs-0001",
            "agent-a",
            EventKind::ConflictResolutionStarted {
                conflicts: vec![],
                new_base: None,
            },
            t,
        ),
    ];
    let projection = Projection::from_events(&events);

    let cs = projection
        .changeset(&ChangesetId("cs-0001".into()))
        .unwrap();
    assert_eq!(
        cs.status,
        ChangesetStatus::Resolving,
        "ConflictResolutionStarted should transition status to Resolving"
    );

    // latest_conflicted_changeset should NOT find it
    assert!(
        projection
            .latest_conflicted_changeset(&AgentId("agent-a".into()))
            .is_none(),
        "Resolving changeset should not appear as conflicted"
    );

    // latest_resolving_changeset should find it
    let resolving = projection
        .latest_resolving_changeset(&AgentId("agent-a".into()))
        .unwrap();
    assert_eq!(resolving.id.0, "cs-0001");
}

#[test]
fn resolving_changeset_can_be_resubmitted() {
    let t = Utc::now();
    let events = vec![
        make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "task".into(),
            },
            t,
        ),
        make_event(
            2,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetConflicted { conflicts: vec![] },
            t,
        ),
        make_event(
            3,
            "cs-0001",
            "agent-a",
            EventKind::ConflictResolutionStarted {
                conflicts: vec![],
                new_base: None,
            },
            t,
        ),
        // After resolution agent finishes, post-session resubmits
        make_event(
            4,
            "cs-0001",
            "agent-a",
            EventKind::ChangesetSubmitted { operations: vec![] },
            t,
        ),
    ];
    let projection = Projection::from_events(&events);

    let cs = projection
        .changeset(&ChangesetId("cs-0001".into()))
        .unwrap();
    assert_eq!(
        cs.status,
        ChangesetStatus::Submitted,
        "Resolving changeset should transition to Submitted after resubmission"
    );
}
