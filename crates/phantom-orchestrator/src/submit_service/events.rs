//! Record the `ChangesetSubmitted` event.
//!
//! **Invariant (H-ORC2):** this must run *after* a successful materialization
//! so that a failed materialize does not leave an orphan submission event.

use chrono::Utc;

use phantom_core::changeset::SemanticOperation;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::error::OrchestratorError;

/// Append a `ChangesetSubmitted` event to the store.
pub(super) async fn record_changeset_submitted(
    events: &dyn EventStore,
    changeset_id: ChangesetId,
    agent_id: &AgentId,
    operations: Vec<SemanticOperation>,
) -> Result<(), OrchestratorError> {
    // Do not silently produce a root-linked event on transient query failure —
    // a broken causal chain corrupts rollback ordering.
    let causal_parent = events
        .latest_event_for_changeset(&changeset_id)
        .await
        .map_err(|e| {
            tracing::warn!(
                error = %e,
                changeset = %changeset_id.0,
                "causal parent query failed"
            );
            OrchestratorError::EventStore(e.to_string())
        })?;
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id,
        agent_id: agent_id.clone(),
        causal_parent,
        kind: EventKind::ChangesetSubmitted { operations },
    };
    events
        .append(event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;
    Ok(())
}
