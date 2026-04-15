//! Submit service — extract semantic operations from an agent's overlay,
//! commit them to trunk via semantic merge, and ripple changes to other agents.
//!
//! This module provides the unified submit-and-materialize pipeline: in a
//! single call it extracts semantic operations, records the submission event,
//! performs the three-way merge, commits to trunk, and runs ripple/live-rebase
//! on active agent overlays.

use std::path::{Path, PathBuf};

use chrono::Utc;

use phantom_core::changeset::{Changeset, ChangesetStatus, SemanticOperation};
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::{EventStore, SemanticAnalyzer};
use phantom_overlay::OverlayLayer;

use crate::error::OrchestratorError;
use crate::git::GitOps;
use crate::materialization_service::{self, ActiveOverlay, MaterializeOutput};
use crate::materializer::Materializer;
use crate::ripple;

/// Output of the submission step (semantic operation extraction).
#[derive(Debug)]
pub struct SubmitOutput {
    /// The changeset ID that was submitted.
    pub changeset_id: ChangesetId,
    /// Number of symbol additions detected.
    pub additions: u32,
    /// Number of symbol modifications detected.
    pub modifications: u32,
    /// Number of symbol deletions detected.
    pub deletions: u32,
    /// List of modified files in the overlay.
    pub modified_files: Vec<PathBuf>,
}

/// Combined output of the unified submit-and-materialize pipeline.
#[derive(Debug)]
pub struct SubmitAndMaterializeOutput {
    /// Submission stats (semantic operations extracted).
    pub submit: SubmitOutput,
    /// Materialization result (merge, commit, ripple effects).
    pub materialize: MaterializeOutput,
}

/// Submit an agent's overlay changes and materialize them to trunk in one step.
///
/// This is the unified pipeline that:
/// 1. Extracts semantic operations from each modified file
/// 2. Appends a `ChangesetSubmitted` event (audit record)
/// 3. Runs the three-way semantic merge and commits to trunk
/// 4. Runs ripple checking and live rebase on other active agents
///
/// Returns `Ok(Some(output))` if changes were found and processed, or
/// `Ok(None)` if the overlay has no modifications.
#[allow(clippy::too_many_arguments)]
pub async fn submit_and_materialize(
    git: &GitOps,
    events: &dyn EventStore,
    analyzer: &dyn SemanticAnalyzer,
    agent_id: &AgentId,
    layer: &OverlayLayer,
    upper_dir: &Path,
    phantom_dir: &Path,
    materializer: &Materializer,
    active_overlays: &[ActiveOverlay],
    message: &str,
) -> Result<Option<SubmitAndMaterializeOutput>, OrchestratorError> {
    let modified = layer
        .modified_files()
        .map_err(|e| OrchestratorError::Overlay(e.to_string()))?;

    if modified.is_empty() {
        return Ok(None);
    }

    // Find the changeset ID and base commit for this agent from events.
    let agent_events = events
        .query_by_agent(agent_id)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;

    let (changeset_id, base_commit) = agent_events
        .iter()
        .rev()
        .find_map(|e| {
            if let EventKind::TaskCreated { base_commit, .. } = &e.kind {
                Some((e.changeset_id.clone(), *base_commit))
            } else {
                None
            }
        })
        .ok_or_else(|| {
            OrchestratorError::NotFound(format!(
                "no overlay found for agent '{agent_id}' — was it tasked?"
            ))
        })?;

    // If a conflict resolution updated the base, use the new base so that
    // the post-resolution submit doesn't re-detect the same symbol conflict
    // against a stale base commit.
    let base_commit = agent_events
        .iter()
        .rev()
        .find_map(|e| {
            if e.changeset_id == changeset_id
                && let EventKind::ConflictResolutionStarted {
                    new_base: Some(base),
                    ..
                } = &e.kind
                {
                    return Some(*base);
                }
            None
        })
        .unwrap_or(base_commit);

    // Extract task description from events for the changeset.
    let task = agent_events
        .iter()
        .find_map(|e| {
            if let EventKind::TaskCreated { task, .. } = &e.kind {
                Some(task.clone())
            } else {
                None
            }
        })
        .unwrap_or_default();

    let mut all_ops: Vec<SemanticOperation> = Vec::new();
    let mut additions = 0u32;
    let mut modifications = 0u32;
    let mut deletions = 0u32;

    for file in &modified {
        let agent_content = std::fs::read(upper_dir.join(file)).map_err(OrchestratorError::Io)?;

        let base_content = git.read_file_at_commit(&base_commit, file);

        let ops = if let Ok(base) = base_content {
            let base_symbols = if let Ok(syms) = analyzer.extract_symbols(file, &base) { syms } else {
                tracing::debug!(?file, "no semantic analysis available for base");
                Vec::new()
            };
            let current_symbols = if let Ok(syms) = analyzer.extract_symbols(file, &agent_content) { syms } else {
                tracing::debug!(?file, "no semantic analysis available for current");
                Vec::new()
            };
            analyzer.diff_symbols(&base_symbols, &current_symbols)
        } else {
            // New file -- all symbols are additions.
            let symbols = if let Ok(syms) = analyzer.extract_symbols(file, &agent_content) { syms } else {
                tracing::debug!(?file, "no semantic analysis available for new file");
                Vec::new()
            };
            symbols
                .into_iter()
                .map(|sym| SemanticOperation::AddSymbol {
                    file: file.clone(),
                    symbol: sym,
                })
                .collect()
        };

        for op in &ops {
            match op {
                SemanticOperation::AddSymbol { .. } | SemanticOperation::AddFile { .. } => {
                    additions += 1;
                }
                SemanticOperation::ModifySymbol { .. } | SemanticOperation::RawDiff { .. } => {
                    modifications += 1;
                }
                SemanticOperation::DeleteSymbol { .. } | SemanticOperation::DeleteFile { .. } => {
                    deletions += 1;
                }
            }
        }

        all_ops.extend(ops);
    }

    // If semantic analysis yielded no structured ops, record raw diffs.
    if all_ops.is_empty() && !modified.is_empty() {
        for file in &modified {
            all_ops.push(SemanticOperation::RawDiff {
                path: file.clone(),
                patch: String::new(),
            });
            modifications += 1;
        }
    }

    // Record the submission event (audit trail of what the agent produced).
    let causal_parent = events
        .latest_event_for_changeset(&changeset_id)
        .await
        .unwrap_or(None);
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent,
        kind: EventKind::ChangesetSubmitted {
            operations: all_ops.clone(),
        },
    };
    events
        .append(event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;

    // Remove stale trunk notification and markdown update.
    ripple::remove_trunk_notification(phantom_dir, agent_id);
    crate::trunk_update::remove_trunk_update_md(upper_dir);

    let submit_output = SubmitOutput {
        changeset_id: changeset_id.clone(),
        additions,
        modifications,
        deletions,
        modified_files: modified.clone(),
    };

    // Build a Changeset struct for the materializer.
    let changeset = Changeset {
        id: changeset_id,
        agent_id: agent_id.clone(),
        task,
        base_commit,
        files_touched: modified,
        operations: all_ops,
        test_result: None,
        created_at: Utc::now(),
        status: ChangesetStatus::Submitted,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    };

    // Materialize: three-way merge, commit to trunk, ripple to other agents.
    let materialize_output = materialization_service::materialize_and_ripple(
        &changeset,
        upper_dir,
        events,
        analyzer,
        materializer,
        phantom_dir,
        active_overlays,
        message,
    )
    .await?;

    Ok(Some(SubmitAndMaterializeOutput {
        submit: submit_output,
        materialize: materialize_output,
    }))
}
