//! Replay engine for rollback support.
//!
//! [`ReplayEngine`] queries the event log to determine which changesets
//! have been materialized and their relative ordering, enabling surgical
//! rollback and selective replay.

use phantom_core::id::ChangesetId;
use sqlx::Row;

use crate::error::EventStoreError;
use crate::store::SqliteEventStore;

/// JSON pattern that identifies `ChangesetMaterialized` events in the `kind`
/// column. The serialized form always starts with `{"ChangesetMaterialized"`.
const MATERIALIZED_KIND_PREFIX: &str = r#"{"ChangesetMaterialized"%"#;

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
    pub async fn materialized_changesets(&self) -> Result<Vec<ChangesetId>, EventStoreError> {
        let rows = sqlx::query(
            "SELECT changeset_id FROM events
             WHERE dropped = 0 AND kind LIKE $1
             ORDER BY id ASC",
        )
        .bind(MATERIALIZED_KIND_PREFIX)
        .fetch_all(&self.store.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| ChangesetId(r.get("changeset_id")))
            .collect())
    }

    /// Return all materialized changeset IDs that were materialized *after*
    /// the given changeset.
    pub async fn changesets_after(
        &self,
        id: &ChangesetId,
    ) -> Result<Vec<ChangesetId>, EventStoreError> {
        // Find the event ID of the target changeset's materialization.
        let target_row = sqlx::query(
            "SELECT id FROM events
             WHERE dropped = 0 AND changeset_id = $1 AND kind LIKE $2
             LIMIT 1",
        )
        .bind(&id.0)
        .bind(MATERIALIZED_KIND_PREFIX)
        .fetch_optional(&self.store.pool)
        .await?;

        let Some(target_row) = target_row else {
            return Ok(Vec::new());
        };
        let target_id: i64 = target_row.get("id");

        // Collect all materialized changesets with higher event IDs.
        let rows = sqlx::query(
            "SELECT changeset_id FROM events
             WHERE dropped = 0 AND id > $1 AND kind LIKE $2
             ORDER BY id ASC",
        )
        .bind(target_id)
        .bind(MATERIALIZED_KIND_PREFIX)
        .fetch_all(&self.store.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| ChangesetId(r.get("changeset_id")))
            .collect())
    }
}
