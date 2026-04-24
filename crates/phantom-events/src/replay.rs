//! Replay engine for rollback support.
//!
//! [`ReplayEngine`] queries the event log to determine which changesets
//! have been materialized and their relative ordering, enabling surgical
//! rollback and selective replay.

use phantom_core::event::{EventKind, MaterializationPath};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use sqlx::Row;

use crate::error::EventStoreError;
use crate::kind_pattern;
use crate::store::SqliteEventStore;

/// An unresolved pre-commit fence event — the materializer emitted
/// [`EventKind::ChangesetMaterializationStarted`] but no subsequent
/// terminal event (`ChangesetMaterialized`, `ChangesetConflicted`, or
/// `ChangesetDropped`) was appended for the changeset. Indicates the
/// submit pipeline crashed somewhere between intent and completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanFence {
    /// ID of the fence event itself.
    pub fence_event_id: EventId,
    /// Changeset that was mid-materialization.
    pub changeset_id: ChangesetId,
    /// Agent that owned the changeset.
    pub agent_id: AgentId,
    /// Trunk HEAD the materializer intended to commit on top of.
    pub parent: GitOid,
    /// Which apply path was running (informational).
    pub path: MaterializationPath,
}

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
        .bind(kind_pattern::materialized_prefix())
        .fetch_all(&self.store.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| ChangesetId(r.get("changeset_id")))
            .collect())
    }

    /// Return fence events that have no subsequent terminal event for their
    /// changeset.
    ///
    /// A crash between the fence append and the terminal event
    /// (`ChangesetMaterialized` / `ChangesetConflicted` / `ChangesetDropped`)
    /// leaves the changeset's intent recorded in the log but its outcome
    /// ambiguous — trunk may or may not carry the commit. Recovery walks
    /// this list and reconciles each entry against git HEAD.
    pub async fn orphan_materialization_fences(
        &self,
    ) -> Result<Vec<OrphanFence>, EventStoreError> {
        // Only consider the *latest* fence per changeset: a successful
        // materialize-then-retry pattern would emit a fence, a terminal,
        // then a second fence for a later attempt. Earlier fences that
        // already have a terminal aren't orphans. The `id > e1.id` check
        // on terminals naturally handles that.
        let rows = sqlx::query(
            "SELECT e1.id, e1.changeset_id, e1.agent_id, e1.kind
             FROM events e1
             WHERE e1.dropped = 0
               AND e1.kind LIKE $1
               AND NOT EXISTS (
                 SELECT 1 FROM events e2
                 WHERE e2.dropped = 0
                   AND e2.changeset_id = e1.changeset_id
                   AND e2.id > e1.id
                   AND (e2.kind LIKE $2 OR e2.kind LIKE $3 OR e2.kind LIKE $4)
               )
             ORDER BY e1.id ASC",
        )
        .bind(kind_pattern::materialization_started_prefix())
        .bind(kind_pattern::materialized_prefix())
        .bind(kind_pattern::conflicted_prefix())
        .bind(kind_pattern::dropped_prefix())
        .fetch_all(&self.store.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row.get("id");
            let changeset_id: String = row.get("changeset_id");
            let agent_id: String = row.get("agent_id");
            let kind_json: String = row.get("kind");

            let event_id = u64::try_from(id).map_err(|_| {
                EventStoreError::CorruptedRow(format!("column 'id' contains negative value {id}"))
            })?;

            // The LIKE filter can't guarantee a matching row decodes to the
            // expected variant — a future binary might serialize a variant
            // whose name shares the prefix. Skip non-fence rows rather than
            // fail the whole recovery scan.
            let kind: EventKind = match serde_json::from_str(&kind_json) {
                Ok(k) => k,
                Err(e) => {
                    tracing::debug!(
                        event_id = id,
                        kind_json,
                        error = %e,
                        "skipping unparseable kind during fence scan"
                    );
                    continue;
                }
            };
            let EventKind::ChangesetMaterializationStarted { parent, path } = kind else {
                continue;
            };

            out.push(OrphanFence {
                fence_event_id: EventId(event_id),
                changeset_id: ChangesetId(changeset_id),
                agent_id: AgentId(agent_id),
                parent,
                path,
            });
        }
        Ok(out)
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
        .bind(kind_pattern::materialized_prefix())
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
        .bind(kind_pattern::materialized_prefix())
        .fetch_all(&self.store.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| ChangesetId(r.get("changeset_id")))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use phantom_core::event::Event;
    use phantom_core::traits::EventStore;

    use crate::store::SqliteEventStore;

    async fn store() -> SqliteEventStore {
        SqliteEventStore::in_memory().await.unwrap()
    }

    fn fence(
        changeset: &str,
        agent: &str,
        parent: GitOid,
        path: MaterializationPath,
    ) -> Event {
        Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: ChangesetId(changeset.into()),
            agent_id: AgentId(agent.into()),
            causal_parent: None,
            kind: EventKind::ChangesetMaterializationStarted { parent, path },
        }
    }

    fn materialized(changeset: &str, agent: &str, oid: GitOid) -> Event {
        Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: ChangesetId(changeset.into()),
            agent_id: AgentId(agent.into()),
            causal_parent: None,
            kind: EventKind::ChangesetMaterialized { new_commit: oid },
        }
    }

    fn dropped(changeset: &str, agent: &str, reason: &str) -> Event {
        Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: ChangesetId(changeset.into()),
            agent_id: AgentId(agent.into()),
            causal_parent: None,
            kind: EventKind::ChangesetDropped {
                reason: reason.into(),
            },
        }
    }

    #[tokio::test]
    async fn orphan_fence_with_no_terminal_is_reported() {
        let s = store().await;
        let parent = GitOid::from_bytes([1; 20]);
        s.append(fence("cs-1", "agent-a", parent, MaterializationPath::Direct))
            .await
            .unwrap();

        let engine = ReplayEngine::new(&s);
        let orphans = engine.orphan_materialization_fences().await.unwrap();

        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].changeset_id, ChangesetId("cs-1".into()));
        assert_eq!(orphans[0].agent_id, AgentId("agent-a".into()));
        assert_eq!(orphans[0].parent, parent);
        assert_eq!(orphans[0].path, MaterializationPath::Direct);
    }

    #[tokio::test]
    async fn fence_followed_by_materialized_is_not_orphan() {
        let s = store().await;
        s.append(fence(
            "cs-1",
            "agent-a",
            GitOid::zero(),
            MaterializationPath::Direct,
        ))
        .await
        .unwrap();
        s.append(materialized("cs-1", "agent-a", GitOid::from_bytes([2; 20])))
            .await
            .unwrap();

        let engine = ReplayEngine::new(&s);
        assert!(engine.orphan_materialization_fences().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fence_followed_by_dropped_is_not_orphan() {
        let s = store().await;
        s.append(fence(
            "cs-1",
            "agent-a",
            GitOid::zero(),
            MaterializationPath::Merge,
        ))
        .await
        .unwrap();
        s.append(dropped("cs-1", "agent-a", "user rollback"))
            .await
            .unwrap();

        let engine = ReplayEngine::new(&s);
        assert!(engine.orphan_materialization_fences().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn second_fence_after_successful_first_is_the_orphan() {
        // Attempt 1: fence + materialized. Attempt 2 (retry after rollback):
        // fence only. Only the second fence should be reported as orphan.
        let s = store().await;
        s.append(fence(
            "cs-1",
            "agent-a",
            GitOid::zero(),
            MaterializationPath::Direct,
        ))
        .await
        .unwrap();
        s.append(materialized("cs-1", "agent-a", GitOid::from_bytes([9; 20])))
            .await
            .unwrap();
        let second_parent = GitOid::from_bytes([10; 20]);
        s.append(fence(
            "cs-1",
            "agent-a",
            second_parent,
            MaterializationPath::Direct,
        ))
        .await
        .unwrap();

        let engine = ReplayEngine::new(&s);
        let orphans = engine.orphan_materialization_fences().await.unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].parent, second_parent);
    }

    #[tokio::test]
    async fn multiple_orphans_returned_in_id_order() {
        let s = store().await;
        s.append(fence(
            "cs-a",
            "agent-a",
            GitOid::from_bytes([1; 20]),
            MaterializationPath::Direct,
        ))
        .await
        .unwrap();
        s.append(fence(
            "cs-b",
            "agent-b",
            GitOid::from_bytes([2; 20]),
            MaterializationPath::Merge,
        ))
        .await
        .unwrap();

        let engine = ReplayEngine::new(&s);
        let orphans = engine.orphan_materialization_fences().await.unwrap();
        assert_eq!(orphans.len(), 2);
        assert_eq!(orphans[0].changeset_id, ChangesetId("cs-a".into()));
        assert_eq!(orphans[1].changeset_id, ChangesetId("cs-b".into()));
        assert!(orphans[0].fence_event_id.0 < orphans[1].fence_event_id.0);
    }
}
