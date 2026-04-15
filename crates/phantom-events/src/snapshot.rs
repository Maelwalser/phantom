//! Projection snapshot persistence and incremental loading.
//!
//! [`SnapshotManager`] avoids replaying the entire event log on every
//! [`Projection`] build by persisting periodic snapshots of the changeset
//! map and replaying only the events that occurred after the snapshot.

use std::collections::HashMap;

use chrono::Utc;
use tracing::debug;

use phantom_core::changeset::Changeset;
use phantom_core::id::{ChangesetId, EventId};

use crate::error::EventStoreError;
use crate::projection::Projection;
use crate::store::SqliteEventStore;

/// Default number of tail events that triggers a new snapshot write.
const DEFAULT_SNAPSHOT_INTERVAL: u64 = 100;

/// A persisted snapshot of projection state at a given event ID.
struct ProjectionSnapshot {
    /// The event ID up to which this snapshot is valid.
    snapshot_at: EventId,
    /// Serialized changeset map.
    changesets: HashMap<ChangesetId, Changeset>,
}

/// Manages projection snapshot lifecycle: load, build, auto-save, invalidate.
pub struct SnapshotManager<'a> {
    store: &'a SqliteEventStore,
    snapshot_interval: u64,
}

impl<'a> SnapshotManager<'a> {
    /// Create a new snapshot manager with the default interval (100 events).
    pub fn new(store: &'a SqliteEventStore) -> Self {
        Self {
            store,
            snapshot_interval: DEFAULT_SNAPSHOT_INTERVAL,
        }
    }

    /// Create a snapshot manager with a custom interval.
    #[cfg(test)]
    pub fn with_interval(store: &'a SqliteEventStore, interval: u64) -> Self {
        Self {
            store,
            snapshot_interval: interval,
        }
    }

    /// Build a [`Projection`] using the most efficient path available:
    ///
    /// 1. Load the latest snapshot (if any).
    /// 2. Replay only events after the snapshot.
    /// 3. If enough new events were replayed, persist a new snapshot.
    pub async fn build_projection(&self) -> Result<Projection, EventStoreError> {
        let snapshot = self.load_latest().await?;

        let (projection, tail_len) = if let Some(snap) = snapshot {
            let tail = self.store.query_after_id(snap.snapshot_at).await?;
            let tail_len = tail.len() as u64;
            debug!(
                snapshot_at = snap.snapshot_at.0,
                tail_events = tail_len,
                "loaded projection from snapshot"
            );
            (Projection::from_snapshot(snap.changesets, &tail), tail_len)
        } else {
            let all = self.store.query_all_events().await?;
            let len = all.len() as u64;
            debug!(total_events = len, "full projection replay (no snapshot)");
            (Projection::from_events(&all), len)
        };

        // Auto-save a new snapshot if we replayed enough events.
        if tail_len >= self.snapshot_interval {
            let latest_id = self.latest_event_id().await?;
            let changesets = projection.clone_changesets();
            if let Err(e) = self.save(latest_id, &changesets).await {
                // Snapshot save failure is non-fatal — we still have
                // a valid in-memory projection.
                tracing::warn!(error = %e, "failed to save projection snapshot");
            } else {
                debug!(snapshot_at = latest_id.0, "saved new projection snapshot");
            }
        }

        Ok(projection)
    }

    /// Delete all snapshots. Called when events are dropped (rollback).
    pub async fn invalidate_all(&self) -> Result<(), EventStoreError> {
        sqlx::query("DELETE FROM projection_snapshots")
            .execute(&self.store.pool)
            .await?;
        debug!("invalidated all projection snapshots");
        Ok(())
    }

    /// Load the most recent snapshot from the database.
    async fn load_latest(&self) -> Result<Option<ProjectionSnapshot>, EventStoreError> {
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
                Ok(Some(ProjectionSnapshot {
                    snapshot_at: EventId(snapshot_at as u64),
                    changesets,
                }))
            }
            None => Ok(None),
        }
    }

    /// Persist a new snapshot.
    async fn save(
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

    /// Find the highest event ID in the store.
    async fn latest_event_id(&self) -> Result<EventId, EventStoreError> {
        let row: (Option<i64>,) = sqlx::query_as("SELECT MAX(id) FROM events WHERE dropped = 0")
            .fetch_one(&self.store.pool)
            .await?;
        Ok(EventId(row.0.unwrap_or(0) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::event::{Event, EventKind};
    use phantom_core::id::{AgentId, ChangesetId, GitOid};

    /// Helper to create a TaskCreated event.
    fn task_event(cs_id: &str, agent_id: &str) -> Event {
        Event {
            id: EventId(0), // Will be assigned by the store.
            timestamp: Utc::now(),
            changeset_id: ChangesetId(cs_id.into()),
            agent_id: AgentId(agent_id.into()),
            causal_parent: None,
            kind: EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: format!("task for {cs_id}"),
            },
        }
    }

    /// Helper to append N task events, each with a unique changeset ID.
    async fn append_n_events(store: &SqliteEventStore, n: usize) {
        for i in 0..n {
            let event = task_event(&format!("cs-{i}"), "agent-a");
            store.append_internal(event).await.unwrap();
        }
    }

    #[tokio::test]
    async fn snapshot_and_full_replay_produce_same_projection() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        append_n_events(&store, 50).await;

        // Full replay.
        let all_events = store.query_all_events().await.unwrap();
        let full = Projection::from_events(&all_events);

        // Take a snapshot at event 30, then replay tail.
        let snapshot_at = EventId(30);
        let events_up_to_30: Vec<_> = all_events
            .iter()
            .filter(|e| e.id.0 <= 30)
            .cloned()
            .collect();
        let base_proj = Projection::from_events(&events_up_to_30);
        let base_changesets = base_proj.into_changesets();

        let tail = store.query_after_id(snapshot_at).await.unwrap();
        let incremental = Projection::from_snapshot(base_changesets, &tail);

        // Both should have the same active agents.
        assert_eq!(full.active_agents(), incremental.active_agents());
        // Both should see all 50 changesets.
        for i in 0..50 {
            let cs_id = ChangesetId(format!("cs-{i}"));
            let f = full.changeset(&cs_id).unwrap();
            let s = incremental.changeset(&cs_id).unwrap();
            assert_eq!(f.status, s.status);
            assert_eq!(f.task, s.task);
        }
    }

    #[tokio::test]
    async fn auto_snapshot_triggers_after_interval() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let mgr = SnapshotManager::with_interval(&store, 10);

        // Append 15 events — should trigger a snapshot on first build.
        append_n_events(&store, 15).await;

        let _proj = mgr.build_projection().await.unwrap();

        // Verify a snapshot was written.
        let snap = mgr.load_latest().await.unwrap();
        assert!(snap.is_some(), "snapshot should have been auto-saved");
        let snap = snap.unwrap();
        assert_eq!(snap.snapshot_at.0, 15);
        assert_eq!(snap.changesets.len(), 15);
    }

    #[tokio::test]
    async fn no_snapshot_when_below_interval() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let mgr = SnapshotManager::with_interval(&store, 100);

        append_n_events(&store, 50).await;
        let _proj = mgr.build_projection().await.unwrap();

        let snap = mgr.load_latest().await.unwrap();
        assert!(
            snap.is_none(),
            "snapshot should not be saved below interval"
        );
    }

    #[tokio::test]
    async fn invalidate_clears_snapshots() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let mgr = SnapshotManager::with_interval(&store, 5);

        append_n_events(&store, 10).await;
        let _proj = mgr.build_projection().await.unwrap();

        // Snapshot exists.
        assert!(mgr.load_latest().await.unwrap().is_some());

        // Invalidate.
        mgr.invalidate_all().await.unwrap();
        assert!(mgr.load_latest().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn incremental_build_uses_snapshot() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let mgr = SnapshotManager::with_interval(&store, 5);

        // First batch — triggers a snapshot.
        append_n_events(&store, 10).await;
        let proj1 = mgr.build_projection().await.unwrap();

        // Add more events (below interval so no new snapshot).
        for i in 10..13 {
            let event = task_event(&format!("cs-{i}"), "agent-b");
            store.append_internal(event).await.unwrap();
        }

        // Second build should use the snapshot + 3 tail events.
        let proj2 = mgr.build_projection().await.unwrap();

        // First projection has 10, second has 13.
        assert_eq!(proj1.active_agents().len(), 1); // agent-a
        assert_eq!(proj2.active_agents().len(), 2); // agent-a + agent-b
    }

    #[tokio::test]
    async fn mark_dropped_invalidates_snapshots() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let mgr = SnapshotManager::with_interval(&store, 5);

        append_n_events(&store, 10).await;
        let _proj = mgr.build_projection().await.unwrap();
        assert!(mgr.load_latest().await.unwrap().is_some());

        // Rollback a changeset.
        store
            .mark_dropped(&ChangesetId("cs-5".into()))
            .await
            .unwrap();

        // Snapshot should be gone.
        assert!(mgr.load_latest().await.unwrap().is_none());
    }
}
