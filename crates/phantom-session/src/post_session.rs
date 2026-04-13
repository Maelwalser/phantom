//! Post-session automation: submit + materialize flow.
//!
//! Shared logic used by both interactive sessions and background agent monitors
//! to auto-submit and auto-materialize changesets after an agent finishes work.

use std::path::Path;

use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::materializer::MaterializeResult;
use phantom_orchestrator::submit_service;
use phantom_overlay::OverlayManager;
use phantom_semantic::SemanticMerger;

use crate::context_file;

/// Handle post-session submit and materialize automation.
///
/// Checks the overlay for modifications and optionally submits and materializes
/// the changeset. Parameters are passed individually rather than through a
/// context object so this function is independent of the CLI layer.
#[allow(clippy::too_many_arguments)]
pub async fn post_session_flow(
    phantom_dir: &Path,
    repo_root: &Path,
    events: &dyn EventStore,
    overlays: &mut OverlayManager,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    auto_submit: bool,
    auto_materialize: bool,
) -> anyhow::Result<()> {
    let layer = overlays.get_layer(agent_id)?;

    let modified = layer.modified_files()?;

    if modified.is_empty() {
        println!("No changes detected in overlay.");
        return Ok(());
    }

    println!("{} file(s) modified in overlay.", modified.len());

    if !auto_submit {
        println!(
            "Run `phantom submit {agent_id}` to submit, then `phantom materialize {changeset_id}` to merge."
        );
        return Ok(());
    }

    // Auto-submit
    println!("Auto-submitting changeset...");
    match submit_overlay(phantom_dir, repo_root, events, overlays, agent_id).await? {
        Some(cs_id) => {
            println!("Changeset {cs_id} submitted.");

            if auto_materialize {
                println!("Auto-materializing...");
                let output = materialize_changeset(
                    phantom_dir,
                    repo_root,
                    events,
                    overlays,
                    &cs_id,
                    &agent_id.0,
                )
                .await?;
                match output.result {
                    MaterializeResult::Success { new_commit } => {
                        let hex = new_commit.to_hex();
                        let short = &hex[..12.min(hex.len())];
                        println!("Materialized {cs_id} -> commit {short}");
                    }
                    MaterializeResult::Conflict { details } => {
                        eprintln!("Materialization failed with {} conflict(s):", details.len());
                        for detail in &details {
                            eprintln!(
                                "  [{:?}] {} -- {}",
                                detail.kind,
                                detail.file.display(),
                                detail.description
                            );
                        }
                        anyhow::bail!("materialization failed due to conflicts");
                    }
                }
            } else {
                println!("Run `phantom materialize {cs_id}` to merge to trunk.");
            }
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

/// Submit an agent's overlay work as a changeset.
///
/// Returns `Some(changeset_id)` if changes were found and submitted,
/// or `None` if the overlay has no modifications.
async fn submit_overlay(
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

    let output = submit_service::submit_overlay(
        &git,
        events,
        &analyzer,
        agent_id,
        layer,
        upper_dir,
        phantom_dir,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    match output {
        Some(out) => {
            println!(
                "  {} additions, {} modifications, {} deletions across {} file(s)",
                out.additions,
                out.modifications,
                out.deletions,
                out.modified_files.len()
            );
            for f in &out.modified_files {
                println!("    {}", f.display());
            }
            Ok(Some(out.changeset_id))
        }
        None => Ok(None),
    }
}

/// Materialize a changeset to trunk.
async fn materialize_changeset(
    phantom_dir: &Path,
    repo_root: &Path,
    events: &dyn EventStore,
    overlays: &OverlayManager,
    changeset_id: &ChangesetId,
    message: &str,
) -> anyhow::Result<phantom_orchestrator::materialization_service::MaterializeOutput> {
    use phantom_core::event::EventKind;
    use phantom_events::Projection;
    use phantom_orchestrator::materialization_service::{self, ActiveOverlay};
    use phantom_orchestrator::materializer::Materializer;

    let all_events = events.query_all().await?;
    let projection = Projection::from_events(&all_events);

    let changeset = projection
        .changeset(changeset_id)
        .ok_or_else(|| anyhow::anyhow!("changeset '{changeset_id}' not found"))?
        .clone();

    let upper_dir = overlays.upper_dir(&changeset.agent_id)?.to_path_buf();

    let materializer = Materializer::new(
        phantom_orchestrator::git::GitOps::open(repo_root)
            .map_err(|e| anyhow::anyhow!("failed to open git repo for materialization: {e}"))?,
    );
    let analyzer = SemanticMerger::new();

    // Build the list of active overlays for ripple checking.
    let active_overlays: Vec<ActiveOverlay> = projection
        .active_agents()
        .into_iter()
        .filter(|a| *a != changeset.agent_id)
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

    let output = materialization_service::materialize_and_ripple(
        &changeset,
        &upper_dir,
        events,
        &analyzer,
        &materializer,
        phantom_dir,
        &active_overlays,
        message,
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(output)
}
