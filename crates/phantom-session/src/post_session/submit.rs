//! Orchestration for submit + materialize.
//!
//! Wraps [`phantom_orchestrator::submit_service::submit_and_materialize`] with
//! the session-layer concerns: opening git/overlay handles, building the
//! ripple neighbour list, and routing results through the display helpers.

use std::path::Path;

use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayManager;
use phantom_semantic::SemanticMerger;

use super::display::{print_conflicts, print_materialize_success, print_submit_summary};

/// Internal result from the submit-and-materialize step.
pub(super) enum SubmitOutcome {
    Submitted { changeset_id: ChangesetId },
    Conflict { changeset_id: ChangesetId },
    NoChanges,
}

/// Submit an agent's overlay work and materialize it to trunk.
pub(super) async fn submit_and_materialize_overlay(
    phantom_dir: &Path,
    repo_root: &Path,
    events: &dyn EventStore,
    overlays: &OverlayManager,
    agent_id: &AgentId,
) -> anyhow::Result<SubmitOutcome> {
    let layer = overlays
        .get_layer(agent_id)
        .map_err(|e| anyhow::anyhow!("no overlay found for agent '{agent_id}': {e}"))?;

    let upper_dir = overlays
        .upper_dir(agent_id)
        .map_err(|e| anyhow::anyhow!("no upper dir for agent '{agent_id}': {e}"))?;

    let git = phantom_git::GitOps::open(repo_root)
        .map_err(|e| anyhow::anyhow!("failed to open git repo: {e}"))?;
    let analyzer = SemanticMerger::new();

    let materializer = Materializer::new(&git);

    // Build the list of active overlays for ripple checking.
    let active_overlays = build_active_overlays(events, overlays, agent_id).await?;

    let output = submit_service::submit_and_materialize(
        &git,
        events,
        &analyzer,
        agent_id,
        layer,
        upper_dir,
        phantom_dir,
        &materializer,
        &active_overlays,
        None,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    match output {
        Some(out) => {
            print_submit_summary(&out);

            match &out.materialize.result {
                MaterializeResult::Success { .. } => {
                    print_materialize_success(&out.submit.changeset_id, &out.materialize.result);
                    Ok(SubmitOutcome::Submitted {
                        changeset_id: out.submit.changeset_id,
                    })
                }
                MaterializeResult::Conflict { details } => {
                    print_conflicts(agent_id, &out.submit.changeset_id, details);
                    Ok(SubmitOutcome::Conflict {
                        changeset_id: out.submit.changeset_id,
                    })
                }
            }
        }
        None => Ok(SubmitOutcome::NoChanges),
    }
}

/// Build the list of active overlays for ripple checking, excluding the
/// submitting agent.
async fn build_active_overlays(
    events: &dyn EventStore,
    overlays: &OverlayManager,
    exclude_agent: &AgentId,
) -> anyhow::Result<Vec<phantom_orchestrator::materialization_service::ActiveOverlay>> {
    use phantom_core::event::EventKind;
    use phantom_events::Projection;
    use phantom_orchestrator::materialization_service::ActiveOverlay;

    let all_events = events.query_all().await?;
    let projection = Projection::from_events(&all_events);

    let active_overlays: Vec<ActiveOverlay> = projection
        .active_agents()
        .into_iter()
        .filter(|a| a != exclude_agent)
        .filter_map(|a| {
            let agent_cs =
                all_events
                    .iter()
                    .filter(|e| e.agent_id == a)
                    .find_map(|e| match &e.kind {
                        EventKind::TaskCreated { .. } => Some(e.changeset_id.clone()),
                        _ => None,
                    });
            let cs_data = agent_cs.and_then(|cs_id| projection.changeset(&cs_id).cloned());
            let agent_upper = overlays
                .upper_dir(&a)
                .ok()
                .map(std::path::Path::to_path_buf);
            match (cs_data, agent_upper) {
                (Some(cs), Some(upper)) => Some(ActiveOverlay {
                    agent_id: a.clone(),
                    files_touched: cs.files_touched.clone(),
                    upper_dir: upper,
                }),
                _ => None,
            }
        })
        .collect();

    Ok(active_overlays)
}
