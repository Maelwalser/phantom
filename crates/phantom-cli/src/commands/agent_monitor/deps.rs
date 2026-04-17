//! Wait for upstream agent dependencies to materialize before starting this
//! agent's CLI process.

use std::path::Path;

use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;

/// Status of a single upstream dependency.
#[allow(dead_code)]
enum DepStatus {
    /// The upstream changeset was materialized to trunk.
    Materialized(GitOid),
    /// The upstream is still in progress (no terminal event yet).
    Pending,
    /// The upstream failed (conflicted, dropped, or agent exited non-zero).
    Failed(String),
}

/// Check the status of a single upstream agent by scanning its events.
async fn check_upstream_status(
    events: &SqliteEventStore,
    upstream: &AgentId,
) -> anyhow::Result<DepStatus> {
    let agent_events = events.query_by_agent(upstream).await?;

    // Walk backwards to find the most recent terminal event.
    for event in agent_events.iter().rev() {
        match &event.kind {
            EventKind::ChangesetMaterialized { new_commit } => {
                return Ok(DepStatus::Materialized(*new_commit));
            }
            EventKind::ChangesetConflicted { .. } => {
                return Ok(DepStatus::Failed(format!(
                    "upstream '{upstream}' has merge conflicts"
                )));
            }
            EventKind::ChangesetDropped { reason } => {
                return Ok(DepStatus::Failed(format!(
                    "upstream '{upstream}' was dropped: {reason}"
                )));
            }
            EventKind::AgentCompleted {
                exit_code,
                materialized,
            } => {
                if *exit_code != Some(0) {
                    let code = exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into());
                    return Ok(DepStatus::Failed(format!(
                        "upstream '{upstream}' failed with exit code {code}"
                    )));
                }
                // Agent completed successfully but hasn't materialized yet —
                // materialization event should follow shortly.
                if !materialized {
                    return Ok(DepStatus::Pending);
                }
            }
            _ => {}
        }
    }

    Ok(DepStatus::Pending)
}

/// Wait for all upstream dependencies to materialize to trunk.
///
/// Emits an `AgentWaitingForDependencies` event, then polls the event store
/// until all upstream agents have a `ChangesetMaterialized` event. Bails if
/// any upstream fails.
pub(super) async fn wait_for_dependencies(
    phantom_dir: &Path,
    events: &SqliteEventStore,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    upstream_agents: &[AgentId],
) -> anyhow::Result<()> {
    // Emit waiting event for observability.
    let causal_parent = events
        .latest_event_for_changeset(changeset_id)
        .await
        .unwrap_or(None);
    let wait_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent,
        kind: EventKind::AgentWaitingForDependencies {
            upstream_agents: upstream_agents.to_vec(),
        },
    };
    events.append(wait_event).await?;

    // Write marker file so `phantom background` / `phantom status` can show
    // the waiting state and the names of upstream agents.
    let waiting_file = phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join("waiting.json");
    let upstream_names: Vec<&str> = upstream_agents.iter().map(|a| a.0.as_str()).collect();
    if let Ok(json) = serde_json::to_string(&upstream_names) {
        let _ = std::fs::write(&waiting_file, json);
    }

    const INITIAL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    const MAX_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
    const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(7200); // 2 hours

    let start = std::time::Instant::now();
    let mut poll_interval = INITIAL_POLL_INTERVAL;

    loop {
        let mut all_satisfied = true;

        for upstream in upstream_agents {
            match check_upstream_status(events, upstream).await? {
                DepStatus::Materialized(_) => {} // satisfied
                DepStatus::Failed(reason) => {
                    anyhow::bail!("dependency failed, cannot start agent '{agent_id}': {reason}");
                }
                DepStatus::Pending => {
                    all_satisfied = false;
                }
            }
        }

        if all_satisfied {
            // Remove the waiting marker — agent is about to start.
            let _ = std::fs::remove_file(&waiting_file);
            return Ok(());
        }

        if start.elapsed() > MAX_WAIT {
            let pending: Vec<&str> = upstream_agents.iter().map(|a| a.0.as_str()).collect();
            anyhow::bail!(
                "timed out waiting for upstream dependencies: {}",
                pending.join(", ")
            );
        }

        tokio::time::sleep(poll_interval).await;
        poll_interval = poll_interval.mul_f32(1.5).min(MAX_POLL_INTERVAL);
    }
}
