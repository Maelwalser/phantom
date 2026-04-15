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
    materializer: &Materializer<'_>,
    active_overlays: &[ActiveOverlay],
    message: Option<&str>,
) -> Result<Option<SubmitAndMaterializeOutput>, OrchestratorError> {
    let all_modified = layer
        .modified_files()
        .map_err(|e| OrchestratorError::Overlay(e.to_string()))?;

    // Filter out gitignored files (node_modules, target/, __pycache__, etc.).
    // Uses the repo's .gitignore rules via git2 — works for all project types.
    let total_count = all_modified.len();
    let modified: Vec<PathBuf> = all_modified
        .into_iter()
        .filter(|path| !git.is_ignored(path).unwrap_or(false))
        .collect();

    let ignored_count = total_count - modified.len();
    if ignored_count > 0 {
        tracing::info!(
            ignored_count,
            "filtered {ignored_count} gitignored file(s) from changeset"
        );
    }

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

    // Build commit message: use explicit message if provided, otherwise
    // generate a descriptive one from the semantic operations.
    let generated;
    let commit_message = match message {
        Some(m) => m,
        None => {
            generated = generate_commit_message(agent_id, &all_ops, &modified);
            &generated
        }
    };

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
        commit_message,
    )
    .await?;

    Ok(Some(SubmitAndMaterializeOutput {
        submit: submit_output,
        materialize: materialize_output,
    }))
}

/// Generate a descriptive commit message from semantic operations.
///
/// Produces a subject line summarizing what changed (symbols and files),
/// followed by a body listing individual operations grouped by file.
fn generate_commit_message(
    agent_id: &AgentId,
    ops: &[SemanticOperation],
    modified_files: &[PathBuf],
) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write;

    // Collect symbol names per action type, grouped by file.
    let mut added: Vec<&str> = Vec::new();
    let mut modified: Vec<&str> = Vec::new();
    let mut deleted: Vec<&str> = Vec::new();
    let mut new_files: Vec<&Path> = Vec::new();
    let mut deleted_files: Vec<&Path> = Vec::new();
    let mut raw_files: Vec<&Path> = Vec::new();

    for op in ops {
        match op {
            SemanticOperation::AddSymbol { symbol, .. } => added.push(&symbol.name),
            SemanticOperation::ModifySymbol { new_entry, .. } => modified.push(&new_entry.name),
            SemanticOperation::DeleteSymbol { id, .. } => {
                // SymbolId is "scope::name::kind", extract the name part.
                let name = id.0.split("::").nth(1).unwrap_or(&id.0);
                deleted.push(name);
            }
            SemanticOperation::AddFile { path } => new_files.push(path),
            SemanticOperation::DeleteFile { path } => deleted_files.push(path),
            SemanticOperation::RawDiff { path, .. } => raw_files.push(path),
        }
    }

    // Build a concise subject line.
    let mut parts: Vec<String> = Vec::new();

    if !modified.is_empty() {
        let names = symbol_summary(&modified, 4);
        parts.push(format!("modify {names}"));
    }
    if !added.is_empty() {
        let names = symbol_summary(&added, 4);
        parts.push(format!("add {names}"));
    }
    if !deleted.is_empty() {
        let names = symbol_summary(&deleted, 4);
        parts.push(format!("remove {names}"));
    }
    if !new_files.is_empty() {
        let names = file_summary(&new_files, 3);
        parts.push(format!("create {names}"));
    }
    if !deleted_files.is_empty() {
        let names = file_summary(&deleted_files, 3);
        parts.push(format!("delete {names}"));
    }
    if !raw_files.is_empty() && parts.is_empty() {
        let names = file_summary(&raw_files, 3);
        parts.push(format!("update {names}"));
    }

    let subject = if parts.is_empty() {
        format!("phantom({agent_id}): update {} file(s)", modified_files.len())
    } else {
        let joined = parts.join(", ");
        // Truncate subject line to keep it readable.
        let subject = format!("phantom({agent_id}): {joined}");
        if subject.len() > 120 {
            format!("{}...", &subject[..117])
        } else {
            subject
        }
    };

    // Build body with per-file breakdown.
    let mut body = String::new();

    // Per-file breakdown in the body.
    let mut file_ops: BTreeMap<&Path, Vec<String>> = BTreeMap::new();
    for op in ops {
        match op {
            SemanticOperation::AddSymbol { file, symbol, .. } => {
                file_ops
                    .entry(file)
                    .or_default()
                    .push(format!("  + {} ({})", symbol.name, symbol.kind));
            }
            SemanticOperation::ModifySymbol { file, new_entry, .. } => {
                file_ops
                    .entry(file)
                    .or_default()
                    .push(format!("  ~ {} ({})", new_entry.name, new_entry.kind));
            }
            SemanticOperation::DeleteSymbol { file, id, .. } => {
                let name = id.0.split("::").nth(1).unwrap_or(&id.0);
                file_ops
                    .entry(file)
                    .or_default()
                    .push(format!("  - {name}"));
            }
            SemanticOperation::AddFile { path } => {
                file_ops
                    .entry(path)
                    .or_default()
                    .push("  (new file)".to_string());
            }
            SemanticOperation::DeleteFile { path } => {
                file_ops
                    .entry(path)
                    .or_default()
                    .push("  (deleted)".to_string());
            }
            SemanticOperation::RawDiff { path, .. } => {
                file_ops
                    .entry(path)
                    .or_default()
                    .push("  (raw diff)".to_string());
            }
        }
    }

    if !file_ops.is_empty() {
        let _ = write!(body, "\n");
        for (file, ops_list) in &file_ops {
            let _ = writeln!(body, "{}:", file.display());
            for line in ops_list {
                let _ = writeln!(body, "{line}");
            }
        }
    }

    if body.is_empty() {
        subject
    } else {
        format!("{subject}{body}")
    }
}

/// Summarize a list of symbol names, showing up to `max` names.
fn symbol_summary(names: &[&str], max: usize) -> String {
    if names.len() <= max {
        names.join(", ")
    } else {
        let shown: Vec<&str> = names[..max].to_vec();
        format!("{} (+{} more)", shown.join(", "), names.len() - max)
    }
}

/// Summarize a list of file paths, showing filenames only.
fn file_summary(paths: &[&Path], max: usize) -> String {
    let names: Vec<&str> = paths
        .iter()
        .map(|p| p.file_name().and_then(|n| n.to_str()).unwrap_or("?"))
        .collect();
    symbol_summary(&names, max)
}
