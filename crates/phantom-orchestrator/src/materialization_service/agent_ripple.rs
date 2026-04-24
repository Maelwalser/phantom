//! Process a ripple on a single agent: classify trunk changes, attempt live
//! rebase on shadowed files, write notifications, and emit audit events.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::{error, warn};

use phantom_core::changeset::SemanticOperation;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid, SymbolId};
use phantom_core::notification::{DependencyImpact, TrunkFileStatus, TrunkNotification};
use phantom_core::traits::{EventStore, SemanticAnalyzer};

use crate::impact;
use crate::live_rebase;
use crate::materializer::Materializer;
use crate::pending_notifications::{self, PendingNotification};
use crate::ripple;

use super::RippleEffect;
use super::notifications::{write_notification_and_base, write_trunk_update};

/// Shared context for ripple processing across all affected agents.
pub(super) struct RippleContext<'a> {
    pub materializer: &'a Materializer<'a>,
    pub analyzer: &'a dyn SemanticAnalyzer,
    pub events: &'a dyn EventStore,
    pub phantom_dir: &'a Path,
    pub changeset_id: &'a ChangesetId,
    pub submitting_agent: &'a AgentId,
    pub changeset_base: &'a GitOid,
    pub head: &'a GitOid,
    pub operations: &'a [SemanticOperation],
    pub trigger_event_id: Option<EventId>,
}

/// Per-agent data for a single ripple.
pub(super) struct AffectedAgent<'a> {
    pub agent_id: &'a AgentId,
    /// Files that both (a) changed on trunk and (b) the agent has some
    /// interest in. These are the files that get classified, live-rebased,
    /// and surfaced in the markdown "file changed" list.
    ///
    /// Empty for dep-only ripples (where the agent never touched anything
    /// trunk changed; only a symbol reference matters).
    pub files: &'a [PathBuf],
    pub upper_path: &'a Path,
    /// The agent's full working-set of files — used to compute dependency
    /// impacts independently from `files`. For file-overlap ripples this is
    /// a superset of `files`; for dep-only ripples it contains files that
    /// trunk did *not* change but whose symbols reference something that
    /// did.
    pub touched_files: &'a [PathBuf],
}

/// Process ripple effects for a single agent.
pub(super) async fn handle_agent_ripple(
    ctx: &RippleContext<'_>,
    target: &AffectedAgent<'_>,
) -> RippleEffect {
    let classified = ripple::classify_trunk_changes(target.files, target.upper_path);
    let shadowed_files: Vec<PathBuf> = classified
        .iter()
        .filter(|(_, s)| *s == TrunkFileStatus::Shadowed)
        .map(|(p, _)| p.clone())
        .collect();

    // Compute per-agent dependency impacts before anything else so they can
    // ship with the notification regardless of which downstream code path
    // runs (no-shadow, rebase success, rebase failure).
    let impacts = compute_agent_impacts(ctx, target);

    if shadowed_files.is_empty() {
        write_notification_and_base(
            ctx.phantom_dir,
            target.agent_id,
            *ctx.head,
            classified.clone(),
            impacts.clone(),
        );
        write_trunk_update(ctx, target, &classified, &impacts);
        enqueue_pending_notification(ctx, target, &classified, &impacts);
        emit_agent_notified(ctx, target, &impacts).await;
        return RippleEffect {
            agent_id: target.agent_id.clone(),
            files: target.files.to_vec(),
            merged_count: 0,
            conflicted_count: 0,
            dep_impact_count: impacts.len(),
        };
    }

    // Do not silently fall back on I/O or parse errors: that would use the
    // *submitting* agent's base as the target agent's base, producing a
    // wrong-base three-way merge that either invents false conflicts or
    // produces silently corrupted output. `Ok(None)` (legitimate missing
    // file) still falls back to the changeset base.
    let old_base = match live_rebase::read_current_base(ctx.phantom_dir, target.agent_id) {
        Ok(Some(base)) => base,
        Ok(None) => *ctx.changeset_base,
        Err(e) => {
            error!(
                agent_id = %target.agent_id,
                error = %e,
                "read_current_base failed; aborting ripple for this agent to avoid wrong-base merge"
            );
            // Still surface the notification so the agent is aware the trunk
            // moved, but do not attempt live rebase with a wrong base.
            write_notification_and_base(
                ctx.phantom_dir,
                target.agent_id,
                *ctx.head,
                classified.clone(),
                impacts.clone(),
            );
            emit_agent_notified(ctx, target, &impacts).await;
            return RippleEffect {
                agent_id: target.agent_id.clone(),
                files: target.files.to_vec(),
                merged_count: 0,
                conflicted_count: shadowed_files.len(),
                dep_impact_count: impacts.len(),
            };
        }
    };

    match live_rebase::rebase_agent(
        ctx.materializer.git(),
        ctx.analyzer,
        target.agent_id,
        &old_base,
        ctx.head,
        target.upper_path,
        &shadowed_files,
    ) {
        Ok(rebase_result) => {
            // The live rebase has already written merged bytes into the
            // agent's upper layer. If we cannot persist `current_base`, the
            // agent's base tracking is permanently wrong — every future
            // ripple would run against the old base and either fabricate
            // conflicts or silently corrupt. Propagate the failure so callers
            // do not see a success-shaped `RippleEffect` for a corrupted
            // state.
            let base_persisted = match live_rebase::write_current_base(
                ctx.phantom_dir,
                target.agent_id,
                ctx.head,
            ) {
                Ok(()) => true,
                Err(e) => {
                    error!(
                        agent_id = %target.agent_id,
                        error = %e,
                        "failed to update current_base after live rebase; agent base tracking is now inconsistent"
                    );
                    false
                }
            };

            // Build enriched notification with rebase outcomes.
            let enriched: Vec<(PathBuf, TrunkFileStatus)> = classified
                .into_iter()
                .map(|(path, status)| {
                    if status == TrunkFileStatus::Shadowed {
                        if rebase_result.merged.contains(&path) {
                            (path, TrunkFileStatus::RebaseMerged)
                        } else {
                            (path, TrunkFileStatus::RebaseConflict)
                        }
                    } else {
                        (path, status)
                    }
                })
                .collect();

            let notif = ripple::build_notification(*ctx.head, enriched.clone(), impacts.clone());
            if let Err(e) =
                ripple::write_trunk_notification(ctx.phantom_dir, target.agent_id, &notif)
            {
                // Escalated from warn! to error!: without this notification the
                // agent has no record of which files were merged vs conflicted
                // during the live rebase and may re-edit already-merged content.
                error!(agent_id = %target.agent_id, error = %e, "failed to write enriched trunk notification after live rebase");
            }

            write_trunk_update(ctx, target, &enriched, &impacts);
            enqueue_pending_notification(ctx, target, &enriched, &impacts);
            emit_agent_notified(ctx, target, &impacts).await;

            let event = Event {
                id: EventId(0),
                timestamp: Utc::now(),
                changeset_id: ctx.changeset_id.clone(),
                agent_id: target.agent_id.clone(),
                causal_parent: ctx.trigger_event_id,
                kind: EventKind::LiveRebased {
                    old_base,
                    new_base: *ctx.head,
                    merged_files: rebase_result.merged.clone(),
                    conflicted_files: rebase_result
                        .conflicted
                        .iter()
                        .map(|(p, _)| p.clone())
                        .collect(),
                },
            };
            if let Err(e) = ctx.events.append(event).await {
                // Escalated from warn! to error!: LiveRebased is the audit
                // entry rollback reads to determine which agents were
                // affected by a given commit. Missing entries can cause
                // incorrect restoration on ph rollback.
                error!(agent_id = %target.agent_id, error = %e, "failed to record live rebase event");
            }

            // If the base-tracking write failed above, report every
            // shadowed file as conflicted so the operator has a signal that
            // subsequent ripples may produce incorrect merges. Counts here
            // feed user-facing summaries and correctness audits.
            let (merged_count, conflicted_count) = if base_persisted {
                (rebase_result.merged.len(), rebase_result.conflicted.len())
            } else {
                (0, shadowed_files.len())
            };
            RippleEffect {
                agent_id: target.agent_id.clone(),
                files: target.files.to_vec(),
                merged_count,
                conflicted_count,
                dep_impact_count: impacts.len(),
            }
        }
        Err(e) => {
            warn!(agent_id = %target.agent_id, error = %e, "live rebase failed");
            write_notification_and_base(
                ctx.phantom_dir,
                target.agent_id,
                *ctx.head,
                classified.clone(),
                impacts.clone(),
            );
            write_trunk_update(ctx, target, &classified, &impacts);
            enqueue_pending_notification(ctx, target, &classified, &impacts);
            emit_agent_notified(ctx, target, &impacts).await;
            RippleEffect {
                agent_id: target.agent_id.clone(),
                files: target.files.to_vec(),
                merged_count: 0,
                conflicted_count: shadowed_files.len(),
                dep_impact_count: impacts.len(),
            }
        }
    }
}

/// Write the per-changeset pending notification that Claude's hook drains on
/// the next turn. Skipped when the ripple has no visible effect on this
/// agent (no changed files *and* no dependency impacts) — there is nothing
/// meaningful to inject and doing so would cost prompt cache space.
///
/// Failures are logged only; the pre-existing file-based notification has
/// already been written, so Claude still has a recoverable path.
fn enqueue_pending_notification(
    ctx: &RippleContext<'_>,
    target: &AffectedAgent<'_>,
    classified: &[(PathBuf, TrunkFileStatus)],
    impacts: &[DependencyImpact],
) {
    if classified.is_empty() && impacts.is_empty() {
        return;
    }

    let notification = TrunkNotification {
        new_commit: *ctx.head,
        timestamp: chrono::Utc::now(),
        files: classified.to_vec(),
        dependency_impacts: impacts.to_vec(),
    };

    let relevant_ops: Vec<SemanticOperation> = ctx
        .operations
        .iter()
        .filter(|op| {
            classified
                .iter()
                .any(|(f, _)| op.file_path() == f.as_path())
        })
        .cloned()
        .collect();

    let summary_md = crate::trunk_update::generate_trunk_update_md(
        ctx.submitting_agent,
        ctx.changeset_id,
        ctx.head,
        &relevant_ops,
        classified,
        impacts,
        ctx.materializer.git(),
    );

    let payload = PendingNotification {
        changeset_id: ctx.changeset_id.clone(),
        submitting_agent: ctx.submitting_agent.clone(),
        notification,
        summary_md,
    };

    if let Err(e) = pending_notifications::write(ctx.phantom_dir, target.agent_id, &payload) {
        warn!(
            agent_id = %target.agent_id,
            error = %e,
            "failed to enqueue pending notification for active delivery",
        );
    }
}

/// Parse the agent's overlapping upper-layer files and compute dependency
/// impacts for the materialized changeset. Enriches each impact's
/// `trunk_preview` with an actual signature diff snippet when the trunk
/// content is available.
///
/// Parsing failures for individual files are logged and skipped — the
/// dependency graph is a best-effort signal, not a correctness boundary.
fn compute_agent_impacts(
    ctx: &RippleContext<'_>,
    target: &AffectedAgent<'_>,
) -> Vec<DependencyImpact> {
    let footprint =
        impact::collect_agent_footprint(ctx.analyzer, target.upper_path, target.touched_files);
    let mut impacts = impact::compute_impacts(ctx.operations, &footprint);

    // Load trunk content for each changed file once, then enrich previews.
    // Cheap: the set of changed files is small (bounded by the submitter's
    // modified files).
    let mut new_contents = impact::TrunkContentMap::new();
    let mut base_contents = impact::TrunkContentMap::new();
    let mut seen = std::collections::HashSet::new();
    for op in ctx.operations {
        let path = op.file_path();
        if !seen.insert(path.to_path_buf()) {
            continue;
        }
        if let Ok(bytes) = ctx.materializer.git().read_file_at_commit(ctx.head, path) {
            new_contents.insert(path.to_path_buf(), bytes);
        }
        if let Ok(bytes) = ctx
            .materializer
            .git()
            .read_file_at_commit(ctx.changeset_base, path)
        {
            base_contents.insert(path.to_path_buf(), bytes);
        }
    }
    impact::enrich_trunk_previews(&mut impacts, ctx.operations, &new_contents, &base_contents);
    impacts
}

/// Emit an [`EventKind::AgentNotified`] event recording the subset of trunk
/// symbols that actually impacted this agent. Uses the pre-existing event
/// variant (event.rs:86-91) that was designed for exactly this purpose.
///
/// Silently skips when `impacts` is empty — no need to spam the event log
/// with no-op notifications for agents whose working set didn't reference
/// any changed trunk symbols.
async fn emit_agent_notified(
    ctx: &RippleContext<'_>,
    target: &AffectedAgent<'_>,
    impacts: &[DependencyImpact],
) {
    if impacts.is_empty() {
        return;
    }
    // Deduplicate symbols — multiple impacts can point at the same trunk
    // symbol via different references.
    let mut seen: std::collections::HashSet<SymbolId> = std::collections::HashSet::new();
    let mut changed_symbols: Vec<SymbolId> = Vec::new();
    for impact in impacts {
        if seen.insert(impact.depends_on.clone()) {
            changed_symbols.push(impact.depends_on.clone());
        }
    }

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: ctx.changeset_id.clone(),
        agent_id: target.agent_id.clone(),
        causal_parent: ctx.trigger_event_id,
        kind: EventKind::AgentNotified {
            agent_id: target.agent_id.clone(),
            changed_symbols,
        },
    };
    if let Err(e) = ctx.events.append(event).await {
        warn!(
            agent_id = %target.agent_id,
            error = %e,
            "failed to record AgentNotified event",
        );
    }
}
