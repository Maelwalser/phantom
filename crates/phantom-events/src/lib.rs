//! `phantom-events` — SQLite-backed append-only event store.
//!
//! Implements [`phantom_core::EventStore`] using SQLite in WAL mode for
//! concurrent readers with a single writer. Provides advanced querying,
//! replay for rollback support, and projection to derive current state
//! from the event log.

pub mod error;
pub mod projection;
pub mod query;
pub mod replay;
pub mod store;

pub use error::EventStoreError;
pub use projection::Projection;
pub use query::EventQuery;
pub use replay::ReplayEngine;
pub use store::SqliteEventStore;

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use phantom_core::changeset::{ChangesetStatus, SemanticOperation, TestResult};
    use phantom_core::event::{Event, EventKind};
    use phantom_core::id::{AgentId, ChangesetId, ContentHash, EventId, GitOid};
    use phantom_core::traits::EventStore;
    use std::path::PathBuf;

    use crate::projection::Projection;
    use crate::query::EventQuery;
    use crate::replay::ReplayEngine;
    use crate::store::SqliteEventStore;

    /// Helper to create an event with the given IDs and kind.
    fn make_event(
        changeset: &str,
        agent: &str,
        kind: EventKind,
        timestamp: chrono::DateTime<Utc>,
    ) -> Event {
        Event {
            id: EventId(0), // placeholder — store assigns real ID
            timestamp,
            changeset_id: ChangesetId(changeset.into()),
            agent_id: AgentId(agent.into()),
            kind,
        }
    }

    // ── Test 1: Round-trip ────────────────────────────────────────────

    #[test]
    fn test_roundtrip_append_and_query() {
        let store = SqliteEventStore::in_memory().unwrap();
        let now = Utc::now();

        // Append 10 events: 5 for cs-1/agent-a, 5 for cs-2/agent-b
        let mut ids = Vec::new();
        for i in 0..5 {
            let id = store
                .append(make_event(
                    "cs-1",
                    "agent-a",
                    EventKind::FileWritten {
                        path: PathBuf::from(format!("file{i}.rs")),
                        content_hash: ContentHash::from_bytes(format!("content-{i}").as_bytes()),
                    },
                    now,
                ))
                .unwrap();
            ids.push(id);
        }
        for i in 0..5 {
            let id = store
                .append(make_event(
                    "cs-2",
                    "agent-b",
                    EventKind::FileWritten {
                        path: PathBuf::from(format!("other{i}.rs")),
                        content_hash: ContentHash::from_bytes(
                            format!("other-content-{i}").as_bytes(),
                        ),
                    },
                    now,
                ))
                .unwrap();
            ids.push(id);
        }

        // query_all returns all 10
        let all = store.query_all().unwrap();
        assert_eq!(all.len(), 10);

        // query_by_changeset returns subset
        let cs1 = store
            .query_by_changeset(&ChangesetId("cs-1".into()))
            .unwrap();
        assert_eq!(cs1.len(), 5);
        assert!(cs1.iter().all(|e| e.changeset_id.0 == "cs-1"));

        let cs2 = store
            .query_by_changeset(&ChangesetId("cs-2".into()))
            .unwrap();
        assert_eq!(cs2.len(), 5);

        // query_by_agent returns subset
        let agent_a = store.query_by_agent(&AgentId("agent-a".into())).unwrap();
        assert_eq!(agent_a.len(), 5);
        assert!(agent_a.iter().all(|e| e.agent_id.0 == "agent-a"));

        let agent_b = store.query_by_agent(&AgentId("agent-b".into())).unwrap();
        assert_eq!(agent_b.len(), 5);

        // Verify content round-trips
        for event in &all {
            assert!(matches!(event.kind, EventKind::FileWritten { .. }));
        }

        // Verify IDs are sequential
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(id.0, (i + 1) as u64);
        }
    }

    // ── Test 2: Mark-dropped ─────────────────────────────────────────

    #[test]
    fn test_mark_dropped_excludes_events() {
        let store = SqliteEventStore::in_memory().unwrap();
        let now = Utc::now();

        // Append events for 3 changesets
        for cs in &["cs-1", "cs-2", "cs-3"] {
            for i in 0..3 {
                store
                    .append(make_event(
                        cs,
                        "agent-a",
                        EventKind::FileWritten {
                            path: PathBuf::from(format!("{cs}-file{i}.rs")),
                            content_hash: ContentHash::from_bytes(b"x"),
                        },
                        now,
                    ))
                    .unwrap();
            }
        }

        assert_eq!(store.query_all().unwrap().len(), 9);

        // Drop cs-2
        let affected = store
            .mark_dropped(&ChangesetId("cs-2".into()))
            .unwrap();
        assert_eq!(affected, 3);

        // query_all excludes dropped
        let remaining = store.query_all().unwrap();
        assert_eq!(remaining.len(), 6);
        assert!(remaining.iter().all(|e| e.changeset_id.0 != "cs-2"));

        // query_by_changeset also excludes dropped
        let cs2 = store
            .query_by_changeset(&ChangesetId("cs-2".into()))
            .unwrap();
        assert_eq!(cs2.len(), 0);
    }

    // ── Test 3: query_since ──────────────────────────────────────────

    #[test]
    fn test_query_since_filters_by_timestamp() {
        let store = SqliteEventStore::in_memory().unwrap();
        let base = Utc::now();

        // Events at base - 2h, base - 1h, base, base + 1h
        let timestamps = vec![
            base - Duration::hours(2),
            base - Duration::hours(1),
            base,
            base + Duration::hours(1),
        ];

        for (i, ts) in timestamps.iter().enumerate() {
            store
                .append(make_event(
                    &format!("cs-{i}"),
                    "agent-a",
                    EventKind::OverlayCreated {
                        base_commit: GitOid::zero(),
                        task: String::new(),
                    },
                    *ts,
                ))
                .unwrap();
        }

        // query_since(base) should return events at base and base+1h
        let since_base = store.query_since(base).unwrap();
        assert_eq!(since_base.len(), 2);

        // query_since(base - 1h) should return 3 events
        let since_minus1 = store.query_since(base - Duration::hours(1)).unwrap();
        assert_eq!(since_minus1.len(), 3);
    }

    // ── Test 4: EventQuery with multiple filters ─────────────────────

    #[test]
    fn test_event_query_intersection() {
        let store = SqliteEventStore::in_memory().unwrap();
        let now = Utc::now();

        // agent-a / cs-1
        store
            .append(make_event(
                "cs-1",
                "agent-a",
                EventKind::OverlayCreated {
                    base_commit: GitOid::zero(),
                    task: String::new(),
                },
                now,
            ))
            .unwrap();
        // agent-a / cs-2
        store
            .append(make_event(
                "cs-2",
                "agent-a",
                EventKind::OverlayCreated {
                    base_commit: GitOid::zero(),
                    task: String::new(),
                },
                now,
            ))
            .unwrap();
        // agent-b / cs-1
        store
            .append(make_event(
                "cs-1",
                "agent-b",
                EventKind::OverlayCreated {
                    base_commit: GitOid::zero(),
                    task: String::new(),
                },
                now,
            ))
            .unwrap();

        // Query with both agent-a AND cs-1
        let results = store
            .query(&EventQuery {
                agent_id: Some(AgentId("agent-a".into())),
                changeset_id: Some(ChangesetId("cs-1".into())),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].agent_id.0, "agent-a");
        assert_eq!(results[0].changeset_id.0, "cs-1");
    }

    // ── Test 5: Projection lifecycle ─────────────────────────────────

    #[test]
    fn test_projection_full_lifecycle() {
        let now = Utc::now();
        let cs_id = "cs-42";
        let agent = "agent-a";

        let events = vec![
            make_event(
                cs_id,
                agent,
                EventKind::OverlayCreated {
                    base_commit: GitOid::zero(),
                    task: String::new(),
                },
                now,
            ),
            make_event(
                cs_id,
                agent,
                EventKind::FileWritten {
                    path: PathBuf::from("src/lib.rs"),
                    content_hash: ContentHash::from_bytes(b"new content"),
                },
                now,
            ),
            make_event(
                cs_id,
                agent,
                EventKind::ChangesetSubmitted {
                    operations: vec![SemanticOperation::AddFile {
                        path: PathBuf::from("src/new.rs"),
                    }],
                },
                now,
            ),
            make_event(
                cs_id,
                agent,
                EventKind::TestsRun(TestResult {
                    passed: 10,
                    failed: 0,
                    skipped: 2,
                }),
                now,
            ),
            make_event(
                cs_id,
                agent,
                EventKind::ChangesetMaterialized {
                    new_commit: GitOid::from_bytes([1; 20]),
                },
                now,
            ),
        ];

        let projection = Projection::from_events(&events);
        let cs = projection
            .changeset(&ChangesetId(cs_id.into()))
            .expect("changeset should exist");

        assert_eq!(cs.status, ChangesetStatus::Materialized);
        assert_eq!(cs.operations.len(), 1);
        assert!(cs.test_result.is_some());
        assert_eq!(cs.test_result.unwrap().passed, 10);
        assert!(cs.files_touched.contains(&PathBuf::from("src/lib.rs")));

        // No active agents (changeset is materialized, not in-progress)
        assert!(projection.active_agents().is_empty());
        assert!(projection.pending_changesets().is_empty());
    }

    // ── Test 6: ReplayEngine ─────────────────────────────────────────

    #[test]
    fn test_replay_engine_changesets_after() {
        let store = SqliteEventStore::in_memory().unwrap();
        let now = Utc::now();

        // Materialize 3 changesets in order
        for cs in &["cs-1", "cs-2", "cs-3"] {
            store
                .append(make_event(
                    cs,
                    "agent-a",
                    EventKind::OverlayCreated {
                        base_commit: GitOid::zero(),
                        task: String::new(),
                    },
                    now,
                ))
                .unwrap();
            store
                .append(make_event(
                    cs,
                    "agent-a",
                    EventKind::ChangesetMaterialized {
                        new_commit: GitOid::from_bytes([1; 20]),
                    },
                    now,
                ))
                .unwrap();
        }

        let engine = ReplayEngine::new(&store);

        // All materialized
        let all = engine.materialized_changesets().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].0, "cs-1");
        assert_eq!(all[1].0, "cs-2");
        assert_eq!(all[2].0, "cs-3");

        // After cs-1: cs-2, cs-3
        let after_1 = engine
            .changesets_after(&ChangesetId("cs-1".into()))
            .unwrap();
        assert_eq!(after_1.len(), 2);
        assert_eq!(after_1[0].0, "cs-2");
        assert_eq!(after_1[1].0, "cs-3");

        // After cs-2: cs-3
        let after_2 = engine
            .changesets_after(&ChangesetId("cs-2".into()))
            .unwrap();
        assert_eq!(after_2.len(), 1);
        assert_eq!(after_2[0].0, "cs-3");

        // After cs-3: empty
        let after_3 = engine
            .changesets_after(&ChangesetId("cs-3".into()))
            .unwrap();
        assert!(after_3.is_empty());

        // Non-existent: empty
        let after_missing = engine
            .changesets_after(&ChangesetId("cs-999".into()))
            .unwrap();
        assert!(after_missing.is_empty());
    }

    // ── Test 7: Empty store ──────────────────────────────────────────

    #[test]
    fn test_empty_store_returns_empty() {
        let store = SqliteEventStore::in_memory().unwrap();
        assert!(store.query_all().unwrap().is_empty());
        assert!(store
            .query_by_changeset(&ChangesetId("cs-1".into()))
            .unwrap()
            .is_empty());
        assert!(store
            .query_by_agent(&AgentId("agent-a".into()))
            .unwrap()
            .is_empty());
        assert!(store.query_since(Utc::now()).unwrap().is_empty());
    }

    // ── Test 8: WAL mode verification ────────────────────────────────

    #[test]
    fn test_wal_mode_enabled() {
        let store = SqliteEventStore::in_memory().unwrap();
        let conn = store.conn.lock().expect("lock poisoned");
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        // In-memory databases report "memory" for journal_mode since WAL
        // doesn't apply to :memory:. For file-backed stores it would be "wal".
        assert!(
            journal_mode == "memory" || journal_mode == "wal",
            "unexpected journal_mode: {journal_mode}"
        );
    }

    #[test]
    fn test_wal_mode_file_backed() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("events.db");
        let store = SqliteEventStore::open(&db_path).unwrap();
        let conn = store.conn.lock().expect("lock poisoned");
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
    }
}
