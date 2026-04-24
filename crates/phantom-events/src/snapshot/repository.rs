//! Persistence layer for projection snapshots.
//!
//! All DB I/O touching the `projection_snapshots` table lives here so the
//! orchestration logic in [`super::SnapshotManager`] can focus on
//! "build a projection" without being coupled to SQL.

use std::collections::HashMap;

use chrono::Utc;

use phantom_core::changeset::Changeset;
use phantom_core::id::{ChangesetId, EventId};

use crate::error::EventStoreError;
use crate::store::SqliteEventStore;
use crate::store::row::checked_id;

/// A persisted snapshot of projection state at a given event ID.
pub(super) struct ProjectionSnapshot {
    /// The event ID up to which this snapshot is valid.
    pub(super) snapshot_at: EventId,
    /// Serialized changeset map.
    pub(super) changesets: HashMap<ChangesetId, Changeset>,
}

/// DB access for the `projection_snapshots` table.
pub(super) struct SnapshotRepository<'a> {
    store: &'a SqliteEventStore,
}

impl<'a> SnapshotRepository<'a> {
    pub(super) fn new(store: &'a SqliteEventStore) -> Self {
        Self { store }
    }

    /// Load the most recent snapshot from the database.
    pub(super) async fn load_latest(&self) -> Result<Option<ProjectionSnapshot>, EventStoreError> {
        let row: Option<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT snapshot_at, data FROM projection_snapshots
             ORDER BY snapshot_at DESC LIMIT 1",
        )
        .fetch_optional(&self.store.pool)
        .await?;

        match row {
            Some((snapshot_at, data)) => {
                let changesets: HashMap<ChangesetId, Changeset> = serde_json::from_slice(&data)
                    .map_err(|e| EventStoreError::SnapshotCorrupted(e.to_string()))?;
                // Reject negative snapshot_at values — a wraparound to a
                // huge u64 would cause `query_after_id` to silently return
                // nothing and mask the corruption as a clean projection.
                let snapshot_at = EventId(checked_id(snapshot_at, "snapshot_at")?);
                Ok(Some(ProjectionSnapshot {
                    snapshot_at,
                    changesets,
                }))
            }
            None => Ok(None),
        }
    }

    /// Persist a new snapshot.
    pub(super) async fn save(
        &self,
        snapshot_at: EventId,
        changesets: &HashMap<ChangesetId, Changeset>,
    ) -> Result<(), EventStoreError> {
        let data = serde_json::to_vec(changesets)
            .map_err(|e| EventStoreError::SnapshotCorrupted(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO projection_snapshots (snapshot_at, data, created_at)
             VALUES ($1, $2, $3)",
        )
        .bind(snapshot_at.0 as i64)
        .bind(&data)
        .bind(&now)
        .execute(&self.store.pool)
        .await?;
        Ok(())
    }

    /// Remove every persisted snapshot. Called when events are dropped.
    pub(super) async fn invalidate_all(&self) -> Result<(), EventStoreError> {
        sqlx::query("DELETE FROM projection_snapshots")
            .execute(&self.store.pool)
            .await?;
        Ok(())
    }

    /// Find the highest event ID in the store (non-dropped only).
    pub(super) async fn latest_event_id(&self) -> Result<EventId, EventStoreError> {
        let row: (Option<i64>,) = sqlx::query_as("SELECT MAX(id) FROM events WHERE dropped = 0")
            .fetch_one(&self.store.pool)
            .await?;
        Ok(EventId(row.0.unwrap_or(0) as u64))
    }
}
