//! Integration test: event log queries across agents, changesets, and time.

mod common;

use std::path::PathBuf;

use chrono::{Duration, Utc};
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, ContentHash, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::EventQuery;

use crate::common::TestContext;

/// Helper to create a test event.
fn make_event(
    changeset: &str,
    agent: &str,
    kind: EventKind,
    timestamp: chrono::DateTime<Utc>,
) -> Event {
    Event {
        id: EventId(0),
        timestamp,
        changeset_id: ChangesetId(changeset.into()),
        agent_id: AgentId(agent.into()),
        kind,
    }
}

#[test]
fn test_event_log_queries() {
    let ctx = TestContext::new();
    let base_time = Utc::now();

    // Generate events across 2 agents and 3 changesets.
    // Agent-a: cs-001 (5 events), cs-002 (3 events)
    // Agent-b: cs-003 (4 events)
    // Total: 12 events

    // cs-001 / agent-a: 5 file-write events
    for i in 0..5 {
        ctx.events
            .append(make_event(
                "cs-001",
                "agent-a",
                EventKind::FileWritten {
                    path: PathBuf::from(format!("cs1-file{i}.rs")),
                    content_hash: ContentHash::from_bytes(format!("cs1-{i}").as_bytes()),
                },
                base_time,
            ))
            .unwrap();
    }

    // cs-002 / agent-a: 3 events
    for i in 0..3 {
        ctx.events
            .append(make_event(
                "cs-002",
                "agent-a",
                EventKind::FileWritten {
                    path: PathBuf::from(format!("cs2-file{i}.rs")),
                    content_hash: ContentHash::from_bytes(format!("cs2-{i}").as_bytes()),
                },
                base_time + Duration::seconds(1),
            ))
            .unwrap();
    }

    // cs-003 / agent-b: 4 events
    for i in 0..4 {
        ctx.events
            .append(make_event(
                "cs-003",
                "agent-b",
                EventKind::FileWritten {
                    path: PathBuf::from(format!("cs3-file{i}.rs")),
                    content_hash: ContentHash::from_bytes(format!("cs3-{i}").as_bytes()),
                },
                base_time + Duration::seconds(2),
            ))
            .unwrap();
    }

    // Add lifecycle events.
    ctx.events
        .append(make_event(
            "cs-001",
            "agent-a",
            EventKind::OverlayCreated {
                base_commit: GitOid::zero(),
                task: String::new(),
            },
            base_time - Duration::seconds(10),
        ))
        .unwrap();

    ctx.events
        .append(make_event(
            "cs-001",
            "agent-a",
            EventKind::ChangesetMaterialized {
                new_commit: GitOid::from_bytes([1; 20]),
            },
            base_time + Duration::seconds(5),
        ))
        .unwrap();

    ctx.events
        .append(make_event(
            "cs-003",
            "agent-b",
            EventKind::ChangesetMaterialized {
                new_commit: GitOid::from_bytes([2; 20]),
            },
            base_time + Duration::seconds(10),
        ))
        .unwrap();

    let total_events = 5 + 3 + 4 + 1 + 1 + 1; // 15

    // --- Query all → verify total count ---
    let all = ctx.events.query_all().unwrap();
    assert_eq!(all.len(), total_events, "should have {total_events} total events");

    // --- Query by agent-a → only agent-a events ---
    let agent_a_events = ctx
        .events
        .query_by_agent(&AgentId("agent-a".into()))
        .unwrap();
    // agent-a has: 5 (cs-001 writes) + 3 (cs-002 writes) + 1 (overlay created) + 1 (materialized) = 10
    assert_eq!(agent_a_events.len(), 10, "agent-a should have 10 events");
    assert!(
        agent_a_events.iter().all(|e| e.agent_id.0 == "agent-a"),
        "all events should belong to agent-a"
    );

    // --- Query by agent-b → only agent-b events ---
    let agent_b_events = ctx
        .events
        .query_by_agent(&AgentId("agent-b".into()))
        .unwrap();
    // agent-b has: 4 (cs-003 writes) + 1 (materialized) = 5
    assert_eq!(agent_b_events.len(), 5, "agent-b should have 5 events");

    // --- Query by cs-002 → only cs-002 events ---
    let cs2_events = ctx
        .events
        .query_by_changeset(&ChangesetId("cs-002".into()))
        .unwrap();
    assert_eq!(cs2_events.len(), 3, "cs-002 should have 3 events");
    assert!(
        cs2_events.iter().all(|e| e.changeset_id.0 == "cs-002"),
        "all events should belong to cs-002"
    );

    // --- Query since a timestamp → correct subset ---
    let since_result = ctx
        .events
        .query_since(base_time + Duration::seconds(1))
        .unwrap();
    // Events at base_time+1s (3), base_time+2s (4), base_time+5s (1), base_time+10s (1) = 9
    assert_eq!(
        since_result.len(),
        9,
        "query_since should return events at or after the threshold"
    );

    // --- Verify event ordering is chronological by ID ---
    let all_ids: Vec<u64> = all.iter().map(|e| e.id.0).collect();
    let mut sorted_ids = all_ids.clone();
    sorted_ids.sort();
    assert_eq!(
        all_ids, sorted_ids,
        "events should be ordered by ID (chronological)"
    );

    // --- Mark cs-001 as dropped → fewer events ---
    let dropped = ctx
        .events
        .mark_dropped(&ChangesetId("cs-001".into()))
        .unwrap();
    assert!(dropped > 0, "should drop at least one event");

    let remaining = ctx.events.query_all().unwrap();
    assert!(
        remaining.len() < total_events,
        "after dropping cs-001, total should decrease"
    );
    assert!(
        remaining.iter().all(|e| e.changeset_id.0 != "cs-001"),
        "no cs-001 events should remain after dropping"
    );

    // --- Query by cs-001 after drop → empty ---
    let cs1_after_drop = ctx
        .events
        .query_by_changeset(&ChangesetId("cs-001".into()))
        .unwrap();
    assert!(
        cs1_after_drop.is_empty(),
        "dropped changeset should return no events"
    );

    // --- Advanced query intersection: agent-a AND cs-002 ---
    let intersection = ctx
        .events
        .query(&EventQuery {
            agent_id: Some(AgentId("agent-a".into())),
            changeset_id: Some(ChangesetId("cs-002".into())),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        intersection.len(),
        3,
        "intersection of agent-a and cs-002 should return 3 events"
    );
    assert!(intersection
        .iter()
        .all(|e| e.agent_id.0 == "agent-a" && e.changeset_id.0 == "cs-002"));
}
