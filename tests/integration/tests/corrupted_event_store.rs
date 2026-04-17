//! Integration test: opening a corrupted SQLite file must fail loudly
//! rather than silently appearing empty or panicking.
//!
//! Pins the invariant that database-level corruption is routed through
//! `EventStoreError` (Sqlx decode failure, schema mismatch, etc.) so the
//! CLI can surface a clear message instead of returning an empty log.

use std::fs;

use phantom_events::SqliteEventStore;

#[tokio::test]
async fn opening_garbage_file_returns_an_error() {
    // Create a temp file and write garbage into it.  SQLite should reject
    // the open; whatever the exact classification, the call must not
    // succeed silently.
    let tmp = tempfile::NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_path_buf();
    fs::write(&path, b"this is not a sqlite database, just garbage bytes")
        .expect("failed to write garbage");

    let result = SqliteEventStore::open(&path).await;

    assert!(
        result.is_err(),
        "opening a corrupted file must return an error, got Ok"
    );
}

#[tokio::test]
async fn opening_empty_file_succeeds_with_fresh_schema() {
    // Sanity check: an *empty* file (zero bytes) is treated by SQLite as a
    // brand-new database. Phantom should initialize its schema on it and
    // the resulting store should be usable.
    let tmp = tempfile::NamedTempFile::new().expect("failed to create temp file");
    let path = tmp.path().to_path_buf();
    // File already exists with zero bytes because NamedTempFile creates it.

    let store = SqliteEventStore::open(&path)
        .await
        .expect("opening an empty file should initialize a fresh schema");
    let count = store.event_count().await.expect("event count query failed");
    assert_eq!(count, 0, "freshly initialized store has no events");
}
