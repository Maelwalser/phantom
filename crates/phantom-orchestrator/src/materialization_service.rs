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

use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, EventId};
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

    let mut ripple_effects = Vec::new();

    // Run ripple check and live rebase on success.
    #[allow(clippy::collapsible_if)]
    if let MaterializeResult::Success { .. } = &result {
        if let Ok(head) = materializer.git().head_oid() {
            if let Ok(changed_files) = materializer
                .git()
                .changed_files(&changeset.base_commit, &head)
            {
                let active: Vec<(AgentId, Vec<PathBuf>)> = active_overlays
                    .iter()
                    .map(|a| (a.agent_id.clone(), a.files_touched.clone()))
                    .collect();

                let affected = RippleChecker::check_ripple(&changed_files, &active);

                for (agent_id, files) in &affected {
                    let upper_path = active_overlays
                        .iter()
                        .find(|a| a.agent_id == *agent_id)
                        .map(|a| a.upper_dir.as_path());

                    let Some(upper_path) = upper_path else {
                        continue;
                    };

                    let classified = ripple::classify_trunk_changes(files, upper_path);
                    let shadowed_files: Vec<PathBuf> = classified
                        .iter()
                        .filter(|(_, s)| *s == TrunkFileStatus::Shadowed)
                        .map(|(p, _)| p.clone())
                        .collect();

                    if shadowed_files.is_empty() {
                        // No shadowed files -- write notification and update base.
                        let notif = ripple::build_notification(head, classified);
                        if let Err(e) =
                            ripple::write_trunk_notification(phantom_dir, agent_id, &notif)
                        {
                            eprintln!("warning: failed to write notification for {agent_id}: {e}");
                        }
                        if let Err(e) =
                            live_rebase::write_current_base(phantom_dir, agent_id, &head)
                        {
                            eprintln!("warning: failed to update current_base for {agent_id}: {e}");
                        }
                        ripple_effects.push(RippleEffect {
                            agent_id: agent_id.clone(),
                            files: files.clone(),
                            merged_count: 0,
                            conflicted_count: 0,
                        });
                        continue;
                    }

                    // Determine the agent's current base commit.
                    let old_base = live_rebase::read_current_base(phantom_dir, agent_id)
                        .ok()
                        .flatten()
                        .unwrap_or(changeset.base_commit);

                    // Attempt live rebase on shadowed files.
                    match live_rebase::rebase_agent(
                        materializer.git(),
                        analyzer,
                        agent_id,
                        &old_base,
                        &head,
                        upper_path,
                        &shadowed_files,
                    ) {
                        Ok(rebase_result) => {
                            // Update current_base to new trunk head.
                            if let Err(e) =
                                live_rebase::write_current_base(phantom_dir, agent_id, &head)
                            {
                                eprintln!(
                                    "warning: failed to update current_base for {agent_id}: {e}"
                                );
                            }

                            // Build enriched notification.
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

                            let notif = ripple::build_notification(head, enriched);
                            if let Err(e) =
                                ripple::write_trunk_notification(phantom_dir, agent_id, &notif)
                            {
                                eprintln!(
                                    "warning: failed to write notification for {agent_id}: {e}"
                                );
                            }

                            // Emit audit event.
                            let event = Event {
                                id: EventId(0),
                                timestamp: Utc::now(),
                                changeset_id: changeset.id.clone(),
                                agent_id: agent_id.clone(),
                                kind: EventKind::LiveRebased {
                                    old_base,
                                    new_base: head,
                                    merged_files: rebase_result.merged.clone(),
                                    conflicted_files: rebase_result
                                        .conflicted
                                        .iter()
                                        .map(|(p, _)| p.clone())
                                        .collect(),
                                },
                            };
                            if let Err(e) = events.append(event).await {
                                eprintln!(
                                    "warning: failed to record live rebase event for {agent_id}: {e}"
                                );
                            }

                            let merged = rebase_result.merged.len();
                            let conflicted = rebase_result.conflicted.len();
                            ripple_effects.push(RippleEffect {
                                agent_id: agent_id.clone(),
                                files: files.clone(),
                                merged_count: merged,
                                conflicted_count: conflicted,
                            });
                        }
                        Err(e) => {
                            // Rebase failed -- fall back to notification-only.
                            eprintln!("warning: live rebase failed for {agent_id}: {e}");
                            let notif = ripple::build_notification(head, classified);
                            if let Err(e) =
                                ripple::write_trunk_notification(phantom_dir, agent_id, &notif)
                            {
                                eprintln!(
                                    "warning: failed to write notification for {agent_id}: {e}"
                                );
                            }
                            ripple_effects.push(RippleEffect {
                                agent_id: agent_id.clone(),
                                files: files.clone(),
                                merged_count: 0,
                                conflicted_count: shadowed_files.len(),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(MaterializeOutput {
        result,
        ripple_effects,
    })
}
