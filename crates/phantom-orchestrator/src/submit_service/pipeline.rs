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
use super::pre_submit_warnings;
use super::scope_audit;
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

    // Pre-submit scope audit: when the agent is part of a plan, compare
    // modified/deleted paths against its PlanDomain scope. Out-of-scope
    // writes are logged here (advisory) so the operator can see them in
    // `ph log --verbose` after submit; tightening to a hard rejection is
    // gated on operational signal in production.
    if let Some(scope) = scope_audit::find_scope(ctx.phantom_dir, ctx.agent_id) {
        let violations = scope_audit::audit_paths(&scope, &changes.modified, &changes.deleted);
        scope_audit::log_violations(ctx.agent_id, &scope, &violations);
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

    // Pre-submit outbound warning: if this changeset contains signature
    // changes or deletions that other active agents reference, surface a
    // heads-up inside the submitter's context file BEFORE materialization
    // ripples out. Advisory only — submit is never blocked.
    pre_submit_warnings::run(
        ctx.analyzer,
        ctx.agent_id,
        ctx.upper_dir,
        &extracted.all_ops,
        ctx.active_overlays,
    );

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
    // If *this* append fails, trunk already holds the commit but no audit
    // event is persisted — recovery relies on git metadata.  The invariant
    // ("trunk moves, no orphan event") is pinned by
    // tests/integration/tests/materialize_append_crash.rs.
    //
    // Only record on clean materialization. When the merge produces a
    // conflict, the materializer has already emitted ChangesetConflicted;
    // appending ChangesetSubmitted afterwards would flip the projection
    // status back to Submitted and hide the conflict from `ph resolve`.
    if matches!(
        materialize_output.result,
        crate::materializer::MaterializeResult::Success { .. }
    ) {
        record_changeset_submitted(
            ctx.events,
            agent_ctx.changeset_id,
            ctx.agent_id,
            extracted.all_ops,
        )
        .await?;
    }

    Ok(Some(SubmitAndMaterializeOutput {
        submit: submit_output,
        materialize: materialize_output,
    }))
}

// NOTE: An earlier iteration of this file contained a `reconcile_current_base`
// helper that tried to cross-check the on-disk `current_base` against the
// event-log base. That helper was removed because the on-disk file is written
// at task creation AND at every live rebase, so "disk differs from event log"
// has no unambiguous meaning — it can mean either "live rebase succeeded but
// event append failed" (disk is newer) or "conflict-resolve advanced the log
// past the disk's last write" (event log is newer). The event log is the
// authoritative source; the fix for the false-conflict bug lives in
// `discovery.rs` (honor `LiveRebased { conflicted_files: [] }` as a base
// advance). If on-disk/event-log drift becomes a real problem, fix it at the
// write side by making `write_current_base` + event append atomic — not by
// trying to paper over divergence at read time.
