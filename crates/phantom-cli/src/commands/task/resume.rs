//! Overlay resumption state machine — recovering changeset ID and base
//! commit from the event log, and classifying the resume status of an
//! existing changeset.

use std::path::Path;

use phantom_core::event::EventKind;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;

/// Generate a unique changeset ID using a UUID v7.
///
/// UUID v7 is time-ordered (embeds a Unix timestamp) with random suffix bits,
/// making collisions practically impossible even under concurrent invocations.
pub(crate) fn generate_changeset_id() -> ChangesetId {
    let id = uuid::Uuid::now_v7();
    ChangesetId(format!("cs-{}", id.as_hyphenated()))
}

/// Recover the changeset ID and base commit for an existing agent overlay from
/// the event log. Finds the most recent `TaskCreated` event for this agent.
pub(super) async fn recover_changeset_from_events(
    events: &SqliteEventStore,
    agent_id: &AgentId,
) -> anyhow::Result<(ChangesetId, GitOid)> {
    let events = events.query_by_agent(agent_id).await?;

    // Walk backwards to find the most recent TaskCreated event.
    for event in events.iter().rev() {
        if let EventKind::TaskCreated { base_commit, .. } = &event.kind {
            return Ok((event.changeset_id.clone(), *base_commit));
        }
    }

    anyhow::bail!(
        "overlay exists for agent '{agent_id}' but no TaskCreated event found in the event log"
    )
}

/// Status of a changeset when attempting to resume a session.
pub(super) enum ResumeStatus {
    /// The changeset is still in-progress — resume normally.
    InProgress,
    /// The changeset was submitted but not yet materialized — resume with the
    /// same changeset (agent can continue editing and re-submit).
    Submitted,
    /// The changeset was materialized — a new changeset should be created for
    /// continued work on this overlay.
    Materialized,
}

/// Check the resume status of a changeset.
///
/// Blocks resume only if the task has been explicitly removed. Otherwise
/// returns the changeset status so the caller can decide whether to reuse the
/// changeset or create a new one.
pub(super) async fn check_changeset_resumable(
    events: &SqliteEventStore,
    cs_id: &ChangesetId,
) -> anyhow::Result<ResumeStatus> {
    let events = events.query_by_changeset(cs_id).await?;

    let mut materialized = false;
    let mut submitted = false;

    for event in &events {
        match &event.kind {
            EventKind::TaskDestroyed => {
                anyhow::bail!(
                    "task for changeset {cs_id} has been removed — \
                     use `phantom <new-agent>` to start fresh"
                );
            }
            EventKind::ChangesetMaterialized { .. } => {
                materialized = true;
            }
            EventKind::ChangesetSubmitted { .. } => {
                submitted = true;
            }
            _ => {}
        }
    }

    if materialized {
        Ok(ResumeStatus::Materialized)
    } else if submitted {
        Ok(ResumeStatus::Submitted)
    } else {
        Ok(ResumeStatus::InProgress)
    }
}

/// Check if a background agent process exists for this agent — either still
/// running (agent.pid with a live process) or finished (agent.status exists).
pub(super) fn has_background_agent(phantom_dir: &Path, agent: &str) -> bool {
    let overlay_dir = phantom_dir.join("overlays").join(agent);

    // A completion marker means a background agent ran.
    if overlay_dir.join("agent.status").exists() {
        return true;
    }

    // A PID file means a background agent was launched (may still be running).
    if overlay_dir.join("agent.pid").exists() {
        return true;
    }

    false
}
