//! Post-session automation: submit + materialize flow.
//!
//! Shared logic used by both interactive sessions and background agent monitors
//! to auto-submit changesets after an agent finishes work. Submit now includes
//! materialization (merge to trunk + ripple to other agents).

use std::path::Path;

use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayManager;
use phantom_semantic::SemanticMerger;

use crate::context_file;

/// Context for post-session submit automation.
///
/// Groups the parameters that [`post_session_flow`] needs, keeping the function
/// independent of the CLI layer while providing a named, self-documenting API.
pub struct PostSessionContext<'a> {
    pub phantom_dir: &'a Path,
    pub repo_root: &'a Path,
    pub events: &'a dyn EventStore,
    pub overlays: &'a mut OverlayManager,
    pub agent_id: &'a AgentId,
    pub changeset_id: &'a ChangesetId,
    pub auto_submit: bool,
}

/// Handle post-session submit automation.
///
/// Checks the overlay for modifications and optionally submits and materializes
/// the changeset in a single step.
pub async fn post_session_flow(ctx: PostSessionContext<'_>) -> anyhow::Result<()> {
    let layer = ctx.overlays.get_layer(ctx.agent_id)?;

    let modified = layer.modified_files()?;

    if modified.is_empty() {
        println!("No changes detected in overlay.");
        return Ok(());
    }

    println!("{} file(s) modified in overlay.", modified.len());

    let agent_id = ctx.agent_id;

    if !ctx.auto_submit {
        println!("Run `phantom submit {agent_id}` to submit and merge to trunk.");
        return Ok(());
    }

    // Auto-submit (which now includes materialization).
    println!("Auto-submitting changeset...");
    match submit_and_materialize_overlay(ctx.phantom_dir, ctx.repo_root, ctx.events, ctx.overlays, agent_id).await? {
        Some(cs_id) => {
            println!("Changeset {cs_id} submitted.");
        }
        None => {
            println!("No changes to submit (files may have been reverted).");
        }
    }

    Ok(())
}

/// Clean up context files from both the work directory and the upper directory.
pub fn cleanup_context_files(work_dir: &Path, overlays: &OverlayManager, agent_id: &AgentId) {
    context_file::cleanup_context_file(work_dir);
    if let Ok(upper_dir) = overlays.upper_dir(agent_id) {
        context_file::cleanup_context_file(upper_dir);
    }
}

// ---------------------------------------------------------------------------
// Internal helpers wrapping orchestrator services
// ---------------------------------------------------------------------------

/// Submit an agent's overlay work and materialize it to trunk.
///
/// Returns `Some(changeset_id)` if changes were found and processed,
/// or `None` if the overlay has no modifications.
async fn submit_and_materialize_overlay(
    phantom_dir: &Path,
    repo_root: &Path,
    events: &dyn EventStore,
    overlays: &OverlayManager,
    agent_id: &AgentId,
) -> anyhow::Result<Option<ChangesetId>> {
    let layer = overlays
        .get_layer(agent_id)
        .map_err(|e| anyhow::anyhow!("no overlay found for agent '{agent_id}': {e}"))?;

    let upper_dir = overlays
        .upper_dir(agent_id)
        .map_err(|e| anyhow::anyhow!("no upper dir for agent '{agent_id}': {e}"))?;

    let git = phantom_orchestrator::git::GitOps::open(repo_root)
        .map_err(|e| anyhow::anyhow!("failed to open git repo: {e}"))?;
    let analyzer = SemanticMerger::new();

    let materializer = Materializer::new(
        phantom_orchestrator::git::GitOps::open(repo_root)
            .map_err(|e| anyhow::anyhow!("failed to open git repo for materialization: {e}"))?,
    );

    // Build the list of active overlays for ripple checking.
    let active_overlays = build_active_overlays(events, overlays, agent_id).await?;

    let message = &agent_id.0;

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
        message,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    match output {
        Some(out) => {
            println!(
                "  {} additions, {} modifications, {} deletions across {} file(s)",
                out.submit.additions,
                out.submit.modifications,
                out.submit.deletions,
                out.submit.modified_files.len()
            );
            for f in &out.submit.modified_files {
                println!("    {}", f.display());
            }

            match out.materialize.result {
                MaterializeResult::Success {
                    new_commit,
                    text_fallback_files,
                } => {
                    let hex = new_commit.to_hex();
                    let short = &hex[..12.min(hex.len())];
                    println!("Submitted {} -> commit {short}", out.submit.changeset_id);
                    if !text_fallback_files.is_empty() {
                        eprintln!(
                            "  Warning: {} file(s) merged via line-based fallback (no syntax validation)",
                            text_fallback_files.len()
                        );
                    }
                }
                MaterializeResult::Conflict { details } => {
                    eprintln!("Submission failed with {} conflict(s):", details.len());
                    for detail in &details {
                        eprintln!(
                            "  [{:?}] {} -- {}",
                            detail.kind,
                            detail.file.display(),
                            detail.description
                        );
                    }
                    eprintln!();
                    eprintln!(
                        "The changeset has been submitted but could not be merged."
                    );
                    eprintln!(
                        "Run `phantom resolve {agent_id}` to attempt resolution, or \
                         `phantom rollback --changeset {}` to drop it.",
                        out.submit.changeset_id
                    );
                    anyhow::bail!("submission failed due to conflicts");
                }
            }

            Ok(Some(out.submit.changeset_id))
        }
        None => Ok(None),
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
            let agent_upper = overlays.upper_dir(&a).ok().map(|p| p.to_path_buf());
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
