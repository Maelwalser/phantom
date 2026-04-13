//! `phantom submit` — submit an agent's work as a changeset.

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::submit_service;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct SubmitArgs {
    /// Agent identifier whose work to submit
    pub agent: String,
}

pub async fn run(args: SubmitArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let overlays = ctx.open_overlays_restored()?;
    let agent_id = AgentId(args.agent.clone());

    match submit_agent(&ctx, &events, &overlays, &agent_id).await? {
        Some(changeset_id) => {
            println!("Changeset {changeset_id} submitted.");
        }
        None => {
            println!("No modified files found for agent '{}'.", args.agent);
        }
    }

    Ok(())
}

/// Submit an agent's overlay work as a changeset.
///
/// Returns `Some(changeset_id)` if changes were found and submitted,
/// or `None` if the overlay has no modifications.
pub async fn submit_agent(
    ctx: &PhantomContext,
    events: &dyn EventStore,
    overlays: &phantom_overlay::OverlayManager,
    agent_id: &AgentId,
) -> anyhow::Result<Option<ChangesetId>> {
    let layer = overlays
        .get_layer(agent_id)
        .with_context(|| format!("no overlay found for agent '{agent_id}'"))?;

    let upper_dir = overlays
        .upper_dir(agent_id)
        .with_context(|| format!("no upper dir for agent '{agent_id}'"))?;

    let git = ctx.open_git()?;
    let analyzer = ctx.semantic();

    let output = submit_service::submit_overlay(
        &git,
        events,
        &analyzer,
        agent_id,
        layer,
        upper_dir,
        &ctx.phantom_dir,
    )
    .await?;

    match output {
        Some(out) => {
            // Print conflict warnings if the service detected them.
            // The service itself no longer prints -- that's the CLI's responsibility.
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
