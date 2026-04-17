//! Post-session automation: submit + materialize flow.
//!
//! Shared logic used by both interactive sessions and background agent monitors
//! to auto-submit changesets after an agent finishes work. Submit now includes
//! materialization (merge to trunk + ripple to other agents).
//!
//! Orchestration lives in [`submit`]; terminal output lives in [`display`].
//! This module only wires the public API and context structs.

mod display;
mod submit;

use std::path::Path;

use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_overlay::OverlayManager;

use crate::context_file;
use submit::{SubmitOutcome, submit_and_materialize_overlay};

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

/// Outcome of the post-session submit flow.
///
/// Callers use this to decide follow-up actions (e.g. destroying the overlay
/// after a successful submit).
#[derive(Debug)]
pub enum PostSessionOutcome {
    /// The overlay had no modifications.
    NoChanges,
    /// Changes were detected but `auto_submit` was false.
    PendingSubmit,
    /// Changeset was submitted and successfully materialized to trunk.
    Submitted { changeset_id: ChangesetId },
    /// Changeset was submitted but could not be merged due to conflicts.
    Conflict { changeset_id: ChangesetId },
}

/// Handle post-session submit automation.
///
/// Checks the overlay for modifications and optionally submits and materializes
/// the changeset in a single step. Returns an [`PostSessionOutcome`] so the
/// caller can decide follow-up actions (e.g. destroying the overlay on success).
pub async fn post_session_flow(ctx: PostSessionContext<'_>) -> anyhow::Result<PostSessionOutcome> {
    let layer = ctx.overlays.get_layer(ctx.agent_id)?;

    let modified = layer.modified_files()?;

    if modified.is_empty() {
        println!("No changes detected in overlay.");
        return Ok(PostSessionOutcome::NoChanges);
    }

    println!("{} file(s) modified in overlay.", modified.len());

    let agent_id = ctx.agent_id;

    if !ctx.auto_submit {
        println!("Run `ph submit {agent_id}` to submit and merge to trunk.");
        return Ok(PostSessionOutcome::PendingSubmit);
    }

    // Auto-submit (which now includes materialization).
    println!("Auto-submitting changeset...");
    match submit_and_materialize_overlay(
        ctx.phantom_dir,
        ctx.repo_root,
        ctx.events,
        ctx.overlays,
        agent_id,
    )
    .await?
    {
        SubmitOutcome::Submitted { changeset_id } => {
            println!("Changeset {changeset_id} submitted.");
            Ok(PostSessionOutcome::Submitted { changeset_id })
        }
        SubmitOutcome::Conflict { changeset_id } => {
            Ok(PostSessionOutcome::Conflict { changeset_id })
        }
        SubmitOutcome::NoChanges => {
            println!("No changes to submit (files may have been reverted).");
            Ok(PostSessionOutcome::NoChanges)
        }
    }
}

/// Clean up context files from both the work directory and the upper directory.
pub fn cleanup_context_files(work_dir: &Path, overlays: &OverlayManager, agent_id: &AgentId) {
    context_file::cleanup_context_file(work_dir);
    if let Ok(upper_dir) = overlays.upper_dir(agent_id) {
        context_file::cleanup_context_file(upper_dir);
    }
}
