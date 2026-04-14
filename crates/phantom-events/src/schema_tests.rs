//! Unit tests for schema creation and migration logic.
//!
//! These tests need `pub(crate)` access to the SQLite pool, so they live
//! as a `#[cfg(test)]` module within the crate rather than in `tests/`.

use crate::store::SqliteEventStore;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;

#[tokio::test]
async fn wal_mode_file_backed() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("events.db");
    let store = SqliteEventStore::open(&db_path).await.unwrap();

    let row: (String,) = sqlx::query_as("PRAGMA journal_mode")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(row.0, "wal");
}

#[tokio::test]
async fn schema_meta_table_created_with_version() {
    let store = SqliteEventStore::in_memory().await.unwrap();
    let row: (String,) =
        sqlx::query_as("SELECT value FROM schema_meta WHERE key = 'schema_version'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(row.0, "2", "schema should be at version 2 after migrations");
}

#[tokio::test]
async fn kind_version_column_written() {
    let store = SqliteEventStore::in_memory().await.unwrap();
    let now = Utc::now();

    store
        .append(Event {
            id: EventId(0),
            timestamp: now,
            changeset_id: ChangesetId("cs-1".into()),
            agent_id: AgentId("agent-a".into()),
            kind: EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: String::new(),
            },
        })
        .await
        .unwrap();

    let row: (i32,) = sqlx::query_as("SELECT kind_version FROM events WHERE id = 1")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(row.0, 1);
}

#[tokio::test]
async fn unknown_event_kind_survives_store_roundtrip() {
    let store = SqliteEventStore::in_memory().await.unwrap();

    // Manually insert an event with an unrecognized kind JSON.
    sqlx::query(
        "INSERT INTO events (timestamp, changeset_id, agent_id, kind, kind_version)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Utc::now().to_rfc3339())
    .bind("cs-future")
    .bind("agent-x")
    .bind(r#""SomeFutureVariant""#)
    .bind(99i32)
    .execute(&store.pool)
    .await
    .unwrap();

    let events = store.query_all().await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Unknown);
}

#[tokio::test]
async fn unknown_data_variant_survives_store_roundtrip() {
    let store = SqliteEventStore::in_memory().await.unwrap();

    // Insert an event with a data-carrying variant that this binary
    // doesn't recognize.
    sqlx::query(
        "INSERT INTO events (timestamp, changeset_id, agent_id, kind, kind_version)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Utc::now().to_rfc3339())
    .bind("cs-future")
    .bind("agent-x")
    .bind(r#"{"AgentReassigned":{"id":"123"}}"#)
    .bind(99i32)
    .execute(&store.pool)
    .await
    .unwrap();

    let events = store.query_all().await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Unknown);
}
