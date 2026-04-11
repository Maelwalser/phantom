//! Replay engine for rollback support.
//!
//! [`ReplayEngine`] queries the event log to determine which changesets
//! have been materialized and their relative ordering, enabling surgical
//! rollback and selective replay.

use phantom_core::event::EventKind;
use phantom_core::id::ChangesetId;
use phantom_core::traits::EventStore;

use crate::error::EventStoreError;
use crate::store::SqliteEventStore;

/// Replay engine for querying materialization history.
pub struct ReplayEngine<'a> {
    store: &'a SqliteEventStore,
}

impl<'a> ReplayEngine<'a> {
    /// Create a new replay engine referencing the given store.
    pub fn new(store: &'a SqliteEventStore) -> Self {
        Self { store }
    }

    /// Return the changeset IDs of all materialized changesets, in order.
    pub fn materialized_changesets(&self) -> Result<Vec<ChangesetId>, EventStoreError> {
        let events = self.store.query_all()?;
        let ids: Vec<ChangesetId> = events
            .into_iter()
            .filter(|e| matches!(e.kind, EventKind::ChangesetMaterialized { .. }))
            .map(|e| e.changeset_id)
            .collect();
        Ok(ids)
    }

    /// Return all materialized changeset IDs that were materialized *after*
    /// the given changeset.
    pub fn changesets_after(&self, id: &ChangesetId) -> Result<Vec<ChangesetId>, EventStoreError> {
        let events = self.store.query_all()?;

        // Find the materialization event for the target changeset.
        let target_event_id = events
            .iter()
            .find(|e| {
                e.changeset_id == *id && matches!(e.kind, EventKind::ChangesetMaterialized { .. })
            })
            .map(|e| e.id);

        let Some(target_id) = target_event_id else {
            return Ok(Vec::new());
        };

        // Collect all materialized changesets with higher event IDs.
        let after: Vec<ChangesetId> = events
            .into_iter()
            .filter(|e| {
                e.id.0 > target_id.0 && matches!(e.kind, EventKind::ChangesetMaterialized { .. })
            })
            .map(|e| e.changeset_id)
            .collect();

        Ok(after)
    }
}
