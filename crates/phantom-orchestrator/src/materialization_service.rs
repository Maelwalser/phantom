//! Materialization orchestration service.
//!
//! Coordinates the full materialize-and-ripple pipeline: applies a changeset to
//! trunk, clears the agent's overlay, runs ripple checking on active agents,
//! performs live rebase on shadowed files, and emits audit events.
//!
//! This module extracts the post-materialization orchestration logic that was
//! previously inline in the CLI's `materialize` command.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::warn;

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, EventId, GitOid};
use phantom_core::notification::TrunkFileStatus;
use phantom_core::traits::{EventStore, SemanticAnalyzer};

use crate::error::OrchestratorError;
use crate::live_rebase;
use crate::materializer::{MaterializeResult, Materializer};
use crate::ripple::{self, RippleChecker};

/// Summary of how a ripple affected one agent.
#[derive(Debug)]
pub struct RippleEffect {
    /// The agent affected by the trunk change.
    pub agent_id: AgentId,
    /// Files that overlapped between the materialized changeset and this agent.
    pub files: Vec<PathBuf>,
    /// Number of files that were cleanly merged via live rebase.
    pub merged_count: usize,
    /// Number of files that had conflicts during live rebase.
    pub conflicted_count: usize,
}

/// Output of the full materialize-and-ripple pipeline.
#[derive(Debug)]
pub struct MaterializeOutput {
    /// The underlying materialization result (success or conflict).
    pub result: MaterializeResult,
    /// Ripple effects on active agents (empty if materialization failed or no
    /// agents were affected).
    pub ripple_effects: Vec<RippleEffect>,
}

/// Information about an active agent overlay needed for ripple checking.
pub struct ActiveOverlay {
    /// The agent's identifier.
    pub agent_id: AgentId,
    /// Files the agent has touched (from its changeset).
    pub files_touched: Vec<PathBuf>,
    /// Path to the agent's upper (write) directory.
    pub upper_dir: PathBuf,
}

/// Orchestrate the full materialize-and-ripple pipeline.
///
/// 1. Calls [`Materializer::materialize`] to commit the changeset to trunk.
/// 2. Runs ripple checking against all active agent overlays.
/// 3. For each affected agent, classifies trunk changes, attempts live rebase
///    on shadowed files, writes enriched notifications, and emits audit events.
///
/// The agent's overlay is intentionally preserved after materialization so the
/// session can be resumed. The overlay is only destroyed by `phantom destroy`.
///
/// Returns a [`MaterializeOutput`] containing the materialization result and
/// any ripple effects.
#[allow(clippy::too_many_arguments)]
pub async fn materialize_and_ripple(
    changeset: &phantom_core::changeset::Changeset,
    upper_dir: &Path,
    events: &dyn EventStore,
    analyzer: &dyn SemanticAnalyzer,
    materializer: &Materializer,
    phantom_dir: &Path,
    active_overlays: &[ActiveOverlay],
    message: &str,
) -> Result<MaterializeOutput, OrchestratorError> {
    let result = materializer
        .materialize(changeset, upper_dir, events, analyzer, message)
        .await?;

    let MaterializeResult::Success { .. } = &result else {
        return Ok(MaterializeOutput {
            result,
            ripple_effects: vec![],
        });
    };

    let head = materializer.git().head_oid()?;
    let changed_files = materializer
        .git()
        .changed_files(&changeset.base_commit, &head)?;

    let active: Vec<(AgentId, Vec<PathBuf>)> = active_overlays
        .iter()
        .map(|a| (a.agent_id.clone(), a.files_touched.clone()))
        .collect();

    let affected = RippleChecker::check_ripple(&changed_files, &active);
    let mut ripple_effects = Vec::new();

    for (agent_id, files) in &affected {
        let Some(overlay) = active_overlays.iter().find(|a| a.agent_id == *agent_id) else {
            continue;
        };

        let effect = handle_agent_ripple(
            materializer,
            analyzer,
            events,
            phantom_dir,
            &changeset.id,
            &changeset.base_commit,
            &head,
            agent_id,
            files,
            &overlay.upper_dir,
        )
        .await;

        ripple_effects.push(effect);
    }

    Ok(MaterializeOutput {
        result,
        ripple_effects,
    })
}

/// Process ripple effects for a single agent: classify trunk changes, attempt
/// live rebase on shadowed files, write notifications, and emit audit events.
#[allow(clippy::too_many_arguments)]
async fn handle_agent_ripple(
    materializer: &Materializer,
    analyzer: &dyn SemanticAnalyzer,
    events: &dyn EventStore,
    phantom_dir: &Path,
    changeset_id: &phantom_core::id::ChangesetId,
    changeset_base: &GitOid,
    head: &GitOid,
    agent_id: &AgentId,
    files: &[PathBuf],
    upper_path: &Path,
) -> RippleEffect {
    let classified = ripple::classify_trunk_changes(files, upper_path);
    let shadowed_files: Vec<PathBuf> = classified
        .iter()
        .filter(|(_, s)| *s == TrunkFileStatus::Shadowed)
        .map(|(p, _)| p.clone())
        .collect();

    if shadowed_files.is_empty() {
        write_notification_and_base(phantom_dir, agent_id, *head, classified);
        return RippleEffect {
            agent_id: agent_id.clone(),
            files: files.to_vec(),
            merged_count: 0,
            conflicted_count: 0,
        };
    }

    let old_base = live_rebase::read_current_base(phantom_dir, agent_id)
        .ok()
        .flatten()
        .unwrap_or(*changeset_base);

    match live_rebase::rebase_agent(
        materializer.git(),
        analyzer,
        agent_id,
        &old_base,
        head,
        upper_path,
        &shadowed_files,
    ) {
        Ok(rebase_result) => {
            if let Err(e) = live_rebase::write_current_base(phantom_dir, agent_id, head) {
                warn!(%agent_id, error = %e, "failed to update current_base");
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

            let notif = ripple::build_notification(*head, enriched);
            if let Err(e) = ripple::write_trunk_notification(phantom_dir, agent_id, &notif) {
                warn!(%agent_id, error = %e, "failed to write notification");
            }

            // Emit audit event.
            let event = Event {
                id: EventId(0),
                timestamp: Utc::now(),
                changeset_id: changeset_id.clone(),
                agent_id: agent_id.clone(),
                kind: EventKind::LiveRebased {
                    old_base,
                    new_base: *head,
                    merged_files: rebase_result.merged.clone(),
                    conflicted_files: rebase_result
                        .conflicted
                        .iter()
                        .map(|(p, _)| p.clone())
                        .collect(),
                },
            };
            if let Err(e) = events.append(event).await {
                warn!(%agent_id, error = %e, "failed to record live rebase event");
            }

            RippleEffect {
                agent_id: agent_id.clone(),
                files: files.to_vec(),
                merged_count: rebase_result.merged.len(),
                conflicted_count: rebase_result.conflicted.len(),
            }
        }
        Err(e) => {
            warn!(%agent_id, error = %e, "live rebase failed");
            write_notification_and_base(phantom_dir, agent_id, *head, classified);
            RippleEffect {
                agent_id: agent_id.clone(),
                files: files.to_vec(),
                merged_count: 0,
                conflicted_count: shadowed_files.len(),
            }
        }
    }
}

/// Write a trunk notification file and update the current_base for an agent.
fn write_notification_and_base(
    phantom_dir: &Path,
    agent_id: &AgentId,
    head: GitOid,
    classified: Vec<(PathBuf, TrunkFileStatus)>,
) {
    let notif = ripple::build_notification(head, classified);
    if let Err(e) = ripple::write_trunk_notification(phantom_dir, agent_id, &notif) {
        warn!(%agent_id, error = %e, "failed to write notification");
    }
    if let Err(e) = live_rebase::write_current_base(phantom_dir, agent_id, &head) {
        warn!(%agent_id, error = %e, "failed to update current_base");
    }
}
