//! Conflict auto-retry: on a `PostSessionOutcome::Conflict` result, rebase
//! against current trunk HEAD and re-run the post-completion flow once. Handles
//! the common case where a parallel agent materialized first and the merge
//! would succeed against the updated trunk.

use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_session::post_session::PostSessionOutcome;

use crate::context::PhantomContext;

/// If `result` is a conflict, try to re-run post-completion with the current
/// trunk HEAD as the new base. Returns the (possibly updated) result.
pub(super) async fn maybe_retry_on_conflict(
    events: &SqliteEventStore,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    exit_code: Option<i32>,
    result: anyhow::Result<PostSessionOutcome>,
) -> anyhow::Result<PostSessionOutcome> {
    if !matches!(&result, Ok(PostSessionOutcome::Conflict { .. })) {
        return result;
    }

    tracing::info!(agent = %agent_id, "conflict detected, retrying with updated trunk base");

    let Ok(retry_ctx) = PhantomContext::locate() else {
        return result;
    };
    let Ok(git) = retry_ctx.open_git() else {
        return result;
    };
    let Ok(current_head) = git.head_oid() else {
        return result;
    };

    // Emit ConflictResolutionStarted with the new base so the submit service
    // uses current trunk HEAD as the merge base.
    let causal = events
        .latest_event_for_changeset(changeset_id)
        .await
        .unwrap_or(None);
    let resolution_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent: causal,
        kind: EventKind::ConflictResolutionStarted {
            conflicts: vec![],
            new_base: Some(current_head),
        },
    };
    if events.append(resolution_event).await.is_ok() {
        return super::run_post_completion(agent_id, changeset_id, exit_code).await;
    }

    result
}
