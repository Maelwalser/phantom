//! Event appends for the materializer, with atomic "git commit ⇄ event log"
//! consistency via HEAD rollback on event store failures (C6).

use chrono::Utc;
use tracing::error;

use phantom_core::changeset::Changeset;
use phantom_core::conflict::ConflictDetail;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{EventId, GitOid};
use phantom_core::traits::EventStore;

use crate::error::OrchestratorError;
use crate::git::GitOps;

/// Append a `ChangesetMaterialized` event. On failure, rolls HEAD back to
/// `head_before` to avoid an orphaned commit with no audit trail (C6).
///
/// Returns the new event ID on success. If the event store write fails and
/// rollback succeeds, returns [`OrchestratorError::EventStore`]. If both
/// fail, returns [`OrchestratorError::MaterializationRecoveryFailed`], which
/// indicates the trunk working tree is in an indeterminate state.
pub(super) async fn finalize_with_rollback(
    git: &GitOps,
    event_store: &dyn EventStore,
    changeset: &Changeset,
    head_before: &GitOid,
    new_commit: &GitOid,
) -> Result<EventId, OrchestratorError> {
    match append_materialized_event(event_store, changeset, new_commit).await {
        Ok(id) => Ok(id),
        Err(e) => {
            error!(error = %e, "event store write failed after git commit, rolling back HEAD");
            if let Err(rollback_err) = git.reset_to_commit(head_before) {
                error!(
                    error = %rollback_err,
                    "CRITICAL: failed to roll back HEAD after event store failure"
                );
                return Err(OrchestratorError::MaterializationRecoveryFailed {
                    cause: e.to_string(),
                    recovery_errors: rollback_err.to_string(),
                });
            }
            Err(OrchestratorError::EventStore(e.to_string()))
        }
    }
}

/// Append a `ChangesetMaterialized` event to the store.
///
/// Returns the assigned `EventId` so callers can use it as the `causal_parent`
/// for downstream events (e.g., `LiveRebased`).
pub(super) async fn append_materialized_event(
    event_store: &dyn EventStore,
    changeset: &Changeset,
    new_commit: &GitOid,
) -> Result<EventId, OrchestratorError> {
    let causal_parent = event_store
        .latest_event_for_changeset(&changeset.id)
        .await
        .unwrap_or(None);
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset.id.clone(),
        agent_id: changeset.agent_id.clone(),
        causal_parent,
        kind: EventKind::ChangesetMaterialized {
            new_commit: *new_commit,
        },
    };
    event_store
        .append(event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))
}

/// Append a `ChangesetConflicted` event to the store.
pub(super) async fn append_conflicted_event(
    event_store: &dyn EventStore,
    changeset: &Changeset,
    conflicts: &[ConflictDetail],
) -> Result<(), OrchestratorError> {
    let causal_parent = event_store
        .latest_event_for_changeset(&changeset.id)
        .await
        .unwrap_or(None);
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset.id.clone(),
        agent_id: changeset.agent_id.clone(),
        causal_parent,
        kind: EventKind::ChangesetConflicted {
            conflicts: conflicts.to_vec(),
        },
    };
    event_store
        .append(event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
    Ok(())
}
