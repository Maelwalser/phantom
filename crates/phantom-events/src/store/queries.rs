//! Inherent [`SqliteEventStore`] methods that execute data-access queries.
//!
//! Grouped here (rather than mixed with the constructor and trait impl in
//! `mod.rs`) so the public query surface is easy to scan and reason about.

use phantom_core::event::Event;
use phantom_core::id::{ChangesetId, EventId};

use crate::error::EventStoreError;
use crate::query::EventQuery;

use super::SqliteEventStore;
use super::query_builder::{QueryBuilder, SortDir, apply_event_filters};
use super::row::{checked_id, row_to_event};

impl SqliteEventStore {
    /// Append an event, returning the auto-generated [`EventId`].
    pub(crate) async fn append_internal(&self, event: Event) -> Result<EventId, EventStoreError> {
        let kind_json = serde_json::to_string(&event.kind)?;
        // Use explicit UTC-with-Z format so every stored timestamp sorts
        // lexicographically in insertion order, matching the timestamp
        // index's BTREE ordering. A future `to_rfc3339` call emitting
        // `+00:00` would break that ordering silently.
        let timestamp_str = event
            .timestamp
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let result = sqlx::query(
            "INSERT INTO events (timestamp, changeset_id, agent_id, kind, kind_version, causal_parent)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&timestamp_str)
        .bind(&event.changeset_id.0)
        .bind(&event.agent_id.0)
        .bind(&kind_json)
        .bind(1i32)
        .bind(event.causal_parent.map(|id| id.0 as i64))
        .execute(&self.pool)
        .await?;

        Ok(EventId(checked_id(
            result.last_insert_rowid(),
            "last_insert_rowid",
        )?))
    }

    /// Execute a flexible query against the event store.
    ///
    /// Results are ordered by `id DESC` (newest first) by default, suitable
    /// for "show me the N most recent events" use cases like `phantom log`.
    pub async fn query(&self, q: &EventQuery) -> Result<Vec<Event>, EventStoreError> {
        let mut qb = QueryBuilder::new();
        apply_event_filters(&mut qb, q);
        qb.fetch(&self.pool, SortDir::from(q.order), q.limit).await
    }

    /// Count events matching the query filters (ignores limit).
    pub async fn count(&self, q: &EventQuery) -> Result<u64, EventStoreError> {
        let mut qb = QueryBuilder::new();
        apply_event_filters(&mut qb, q);
        qb.fetch_count(&self.pool).await
    }

    /// Return all causal descendants of the given event (including itself).
    ///
    /// Walks the causal DAG breadth-first using a recursive CTE on the
    /// `causal_parent` column. Results are ordered by `id ASC`.
    pub async fn query_descendants(&self, root: EventId) -> Result<Vec<Event>, EventStoreError> {
        let sql = "
            WITH RECURSIVE descendants(id) AS (
                SELECT id FROM events WHERE id = $1 AND dropped = 0
                UNION ALL
                SELECT e.id FROM events e
                INNER JOIN descendants d ON e.causal_parent = d.id
                WHERE e.dropped = 0
            )
            SELECT e.id, e.timestamp, e.changeset_id, e.agent_id, e.kind, e.causal_parent
            FROM events e
            INNER JOIN descendants d ON e.id = d.id
            ORDER BY e.id ASC
        ";
        let rows = sqlx::query(sql)
            .bind(root.0 as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_event).collect()
    }

    /// Return all non-dropped events in insertion order.
    ///
    /// This is the same as the [`EventStore::query_all`](phantom_core::traits::EventStore::query_all)
    /// trait method but returns [`EventStoreError`] directly, avoiding the
    /// `CoreError` conversion needed by the trait. Used internally by
    /// [`crate::snapshot::SnapshotManager`].
    pub async fn query_all_events(&self) -> Result<Vec<Event>, EventStoreError> {
        self.query_events("dropped = 0", &[]).await
    }

    /// Return the count of non-dropped events.
    pub async fn event_count(&self) -> Result<u64, EventStoreError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events WHERE dropped = 0")
            .fetch_one(&self.pool)
            .await?;
        checked_id(row.0, "count")
    }

    /// Return events whose `id` is strictly greater than `after`, in
    /// insertion order. Used by [`crate::snapshot::SnapshotManager`] to replay
    /// only the tail of the event log after a snapshot.
    pub async fn query_after_id(&self, after: EventId) -> Result<Vec<Event>, EventStoreError> {
        let mut qb = QueryBuilder::new();
        let p = qb.bind((after.0 as i64).to_string());
        qb.push(format!("id > {p}"));
        qb.fetch(&self.pool, SortDir::Asc, None).await
    }

    /// Mark all events belonging to a changeset as dropped.
    ///
    /// Also invalidates all projection snapshots, since they may contain
    /// state derived from the dropped events. Both operations run in a
    /// single transaction so a crash between the UPDATE and the snapshot
    /// wipe cannot leave a stale snapshot masking the dropped state.
    ///
    /// Returns the number of rows affected.
    pub async fn mark_dropped(&self, changeset_id: &ChangesetId) -> Result<u64, EventStoreError> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("UPDATE events SET dropped = 1 WHERE changeset_id = $1")
            .bind(&changeset_id.0)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM projection_snapshots")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }

    /// Read events matching a simple WHERE clause with positional parameters.
    ///
    /// Results are always ordered by `id ASC` (chronological), as expected
    /// by the [`EventStore`](phantom_core::traits::EventStore) trait methods.
    ///
    /// `where_clause` must be caller-constructed from fixed SQL tokens and
    /// positional parameter references (`$1`, `$2`, ...). The string is
    /// interpolated directly into SQL; callers that embed user input there
    /// would create an injection hole. To prevent accidental misuse, the
    /// function is `pub(crate)` and the only callers live inside this crate.
    pub(crate) async fn query_events(
        &self,
        where_clause: &str,
        params: &[String],
    ) -> Result<Vec<Event>, EventStoreError> {
        let mut qb = QueryBuilder::new();
        // `QueryBuilder::new()` seeds `dropped = 0` — we drop that here and
        // trust the caller's clause to include whatever `dropped` filter is
        // appropriate. Parameters are appended in order so `$N` placeholders
        // stay consistent with the existing bind count.
        qb.replace_conditions(vec![where_clause.to_string()]);
        for p in params {
            qb.bind(p.clone());
        }
        qb.fetch(&self.pool, SortDir::Asc, None).await
    }
}
