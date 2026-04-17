//! Unified submit-and-materialize pipeline.
//!
//! Thin orchestrator: collects overlay changes, resolves the agent's context,
//! extracts semantic operations, builds a changeset, runs materialize +
//! ripple, then records the submission event last (see [`events`] — H-ORC2).

use std::path::Path;

use phantom_core::id::AgentId;
use phantom_core::traits::{EventStore, SemanticAnalyzer};
use phantom_overlay::OverlayLayer;

use crate::error::OrchestratorError;
use crate::git::GitOps;
use crate::materialization_service::{self, ActiveOverlay};
use crate::materializer::Materializer;
use crate::ripple;
use crate::trunk_update;

use super::changeset_builder::build_changeset;
use super::commit_message::generate_commit_message;
use super::discovery::resolve_agent_context;
use super::events::record_changeset_submitted;
use super::operations::extract_operations;
use super::overlay_scan::collect_changes;
use super::{SubmitAndMaterializeOutput, SubmitOutput};

/// Bundled inputs for [`submit_and_materialize`].
///
/// Keeps the many collaborators out of the public function signature so the
/// pipeline body stays readable.
pub(super) struct SubmitContext<'a> {
    pub git: &'a GitOps,
    pub events: &'a dyn EventStore,
    pub analyzer: &'a dyn SemanticAnalyzer,
    pub agent_id: &'a AgentId,
    pub layer: &'a OverlayLayer,
    pub upper_dir: &'a Path,
    pub phantom_dir: &'a Path,
    pub materializer: &'a Materializer<'a>,
    pub active_overlays: &'a [ActiveOverlay],
    pub message: Option<&'a str>,
}

/// Execute the submit-and-materialize pipeline.
///
/// Returns `Ok(None)` when the overlay has no changes to submit.
pub(super) async fn run(
    ctx: SubmitContext<'_>,
) -> Result<Option<SubmitAndMaterializeOutput>, OrchestratorError> {
    let changes = collect_changes(ctx.git, ctx.layer)?;
    if changes.is_empty() {
        return Ok(None);
    }

    let agent_ctx = resolve_agent_context(ctx.events, ctx.agent_id).await?;

    let extracted = extract_operations(
        ctx.git,
        ctx.analyzer,
        &agent_ctx.base_commit,
        ctx.upper_dir,
        &changes.modified,
        &changes.deleted,
    )?;

    // Remove stale trunk notification and markdown update BEFORE materializing
    // so concurrent readers do not see a mismatched notification.
    ripple::remove_trunk_notification(ctx.phantom_dir, ctx.agent_id);
    trunk_update::remove_trunk_update_md(ctx.upper_dir);

    // Build commit message: use the explicit one if provided, otherwise
    // synthesize a descriptive one from the semantic operations.
    let generated;
    let commit_message = if let Some(m) = ctx.message {
        m
    } else {
        generated = generate_commit_message(ctx.agent_id, &extracted.all_ops, &changes.modified);
        &generated
    };

    let changeset = build_changeset(
        &agent_ctx.changeset_id,
        ctx.agent_id,
        agent_ctx.task,
        agent_ctx.base_commit,
        &changes.modified,
        &changes.deleted,
        extracted.all_ops.clone(),
    );

    let submit_output = SubmitOutput {
        changeset_id: agent_ctx.changeset_id.clone(),
        additions: extracted.counts.additions,
        modifications: extracted.counts.modifications,
        deletions: extracted.counts.deletions,
        modified_files: changes.modified.clone(),
    };

    let materialize_output = materialization_service::materialize_and_ripple(
        &changeset,
        ctx.upper_dir,
        ctx.events,
        ctx.analyzer,
        ctx.materializer,
        ctx.phantom_dir,
        ctx.active_overlays,
        commit_message,
    )
    .await?;

    // H-ORC2: record the submission event AFTER materialization succeeds to
    // avoid orphaned ChangesetSubmitted events when materialize fails.
    record_changeset_submitted(
        ctx.events,
        agent_ctx.changeset_id,
        ctx.agent_id,
        extracted.all_ops,
    )
    .await?;

    Ok(Some(SubmitAndMaterializeOutput {
        submit: submit_output,
        materialize: materialize_output,
    }))
}
