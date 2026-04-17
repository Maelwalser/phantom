//! Process a ripple on a single agent: classify trunk changes, attempt live
//! rebase on shadowed files, write notifications, and emit audit events.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::warn;

use phantom_core::changeset::SemanticOperation;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::notification::TrunkFileStatus;
use phantom_core::traits::{EventStore, SemanticAnalyzer};

use crate::live_rebase;
use crate::materializer::Materializer;
use crate::ripple;

use super::notifications::{write_notification_and_base, write_trunk_update};
use super::RippleEffect;

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
    pub files: &'a [PathBuf],
    pub upper_path: &'a Path,
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

    if shadowed_files.is_empty() {
        write_notification_and_base(
            ctx.phantom_dir,
            target.agent_id,
            *ctx.head,
            classified.clone(),
        );
        write_trunk_update(ctx, target, &classified);
        return RippleEffect {
            agent_id: target.agent_id.clone(),
            files: target.files.to_vec(),
            merged_count: 0,
            conflicted_count: 0,
        };
    }

    let old_base = live_rebase::read_current_base(ctx.phantom_dir, target.agent_id)
        .ok()
        .flatten()
        .unwrap_or(*ctx.changeset_base);

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
            if let Err(e) =
                live_rebase::write_current_base(ctx.phantom_dir, target.agent_id, ctx.head)
            {
                warn!(agent_id = %target.agent_id, error = %e, "failed to update current_base");
            }

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

            let notif = ripple::build_notification(*ctx.head, enriched.clone());
            if let Err(e) =
                ripple::write_trunk_notification(ctx.phantom_dir, target.agent_id, &notif)
            {
                warn!(agent_id = %target.agent_id, error = %e, "failed to write notification");
            }

            write_trunk_update(ctx, target, &enriched);

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
                warn!(agent_id = %target.agent_id, error = %e, "failed to record live rebase event");
            }

            RippleEffect {
                agent_id: target.agent_id.clone(),
                files: target.files.to_vec(),
                merged_count: rebase_result.merged.len(),
                conflicted_count: rebase_result.conflicted.len(),
            }
        }
        Err(e) => {
            warn!(agent_id = %target.agent_id, error = %e, "live rebase failed");
            write_notification_and_base(
                ctx.phantom_dir,
                target.agent_id,
                *ctx.head,
                classified.clone(),
            );
            write_trunk_update(ctx, target, &classified);
            RippleEffect {
                agent_id: target.agent_id.clone(),
                files: target.files.to_vec(),
                merged_count: 0,
                conflicted_count: shadowed_files.len(),
            }
        }
    }
}
