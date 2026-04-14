//! Submit service — extract semantic operations from an agent's overlay and
//! pre-check for conflicts against trunk.
//!
//! This module extracts the submission and conflict pre-checking logic that was
//! previously inline in the CLI's `submit` command.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::Utc;

use phantom_core::changeset::SemanticOperation;
use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::event::{Event, EventKind, MergeCheckResult};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid, SymbolId};
use phantom_core::traits::{EventStore, SemanticAnalyzer};
use phantom_overlay::OverlayLayer;

use crate::error::OrchestratorError;
use crate::git::GitOps;
use crate::ripple;

/// Output of a successful submission.
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

/// Submit an agent's overlay changes as a changeset.
///
/// Extracts semantic operations from each modified file, pre-checks for
/// conflicts against trunk, appends the submission and merge-check events to
/// the event store, and removes any stale trunk notification.
///
/// Returns `Ok(Some(output))` if changes were found and submitted, or
/// `Ok(None)` if the overlay has no modifications.
pub async fn submit_overlay(
    git: &GitOps,
    events: &dyn EventStore,
    analyzer: &dyn SemanticAnalyzer,
    agent_id: &AgentId,
    layer: &OverlayLayer,
    upper_dir: &Path,
    phantom_dir: &Path,
) -> Result<Option<SubmitOutput>, OrchestratorError> {
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
    // the post-resolution submit and materialize don't re-detect the same
    // symbol conflict against a stale base commit.
    let base_commit = agent_events
        .iter()
        .rev()
        .find_map(|e| {
            if e.changeset_id == changeset_id {
                if let EventKind::ConflictResolutionStarted {
                    new_base: Some(base),
                    ..
                } = &e.kind
                {
                    return Some(*base);
                }
            }
            None
        })
        .unwrap_or(base_commit);

    let mut all_ops: Vec<SemanticOperation> = Vec::new();
    let mut additions = 0u32;
    let mut modifications = 0u32;
    let mut deletions = 0u32;

    for file in &modified {
        let agent_content = std::fs::read(upper_dir.join(file)).map_err(OrchestratorError::Io)?;

        let base_content = git.read_file_at_commit(&base_commit, file);

        let ops = match base_content {
            Ok(base) => {
                let base_symbols = match analyzer.extract_symbols(file, &base) {
                    Ok(syms) => syms,
                    Err(_) => {
                        tracing::debug!(?file, "no semantic analysis available for base");
                        Vec::new()
                    }
                };
                let current_symbols = match analyzer.extract_symbols(file, &agent_content) {
                    Ok(syms) => syms,
                    Err(_) => {
                        tracing::debug!(?file, "no semantic analysis available for current");
                        Vec::new()
                    }
                };
                analyzer.diff_symbols(&base_symbols, &current_symbols)
            }
            Err(_) => {
                // New file -- all symbols are additions.
                let symbols = match analyzer.extract_symbols(file, &agent_content) {
                    Ok(syms) => syms,
                    Err(_) => {
                        tracing::debug!(?file, "no semantic analysis available for new file");
                        Vec::new()
                    }
                };
                symbols
                    .into_iter()
                    .map(|sym| SemanticOperation::AddSymbol {
                        file: file.clone(),
                        symbol: sym,
                    })
                    .collect()
            }
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

    // Early conflict pre-check.
    let merge_check = precheck_conflicts(git, analyzer, &base_commit, &all_ops, &changeset_id);

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::ChangesetSubmitted {
            operations: all_ops,
        },
    };
    events
        .append(event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;

    // Record the merge pre-check result.
    let check_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::ChangesetMergeChecked {
            result: merge_check,
        },
    };
    events
        .append(check_event)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;

    // Remove stale trunk notification.
    ripple::remove_trunk_notification(phantom_dir, agent_id);

    Ok(Some(SubmitOutput {
        changeset_id,
        additions,
        modifications,
        deletions,
        modified_files: modified,
    }))
}

/// Compare the agent's submitted operations against the current trunk state
/// to detect potential symbol-level conflicts early.
///
/// If trunk has advanced since the agent's base commit, this checks whether
/// any symbols the agent modified were also modified on trunk. Returns a
/// [`MergeCheckResult`] that is recorded as an audit event.
fn precheck_conflicts(
    git: &GitOps,
    analyzer: &dyn SemanticAnalyzer,
    base_commit: &GitOid,
    operations: &[SemanticOperation],
    changeset_id: &ChangesetId,
) -> MergeCheckResult {
    let head = match git.head_oid() {
        Ok(h) => h,
        Err(_) => return MergeCheckResult::Clean,
    };

    if head == *base_commit {
        return MergeCheckResult::Clean;
    }

    let agent_by_file = group_ops_by_file(operations);
    let mut symbol_conflicts = Vec::new();

    for (file, agent_syms) in &agent_by_file {
        if agent_syms.is_empty() {
            continue;
        }

        let base = match git.read_file_at_commit(base_commit, file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let trunk = match git.read_file_at_commit(&head, file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if base == trunk {
            continue;
        }

        let base_symbols = analyzer.extract_symbols(file, &base).unwrap_or_default();
        let trunk_symbols = analyzer.extract_symbols(file, &trunk).unwrap_or_default();
        let trunk_ops = analyzer.diff_symbols(&base_symbols, &trunk_symbols);
        let trunk_names: HashSet<String> = trunk_ops
            .iter()
            .filter_map(|op| op.symbol_name().map(String::from))
            .collect();

        for agent_sym in agent_syms {
            if trunk_names.contains(agent_sym.as_str()) {
                symbol_conflicts.push(ConflictDetail {
                    kind: ConflictKind::BothModifiedSymbol,
                    file: file.clone(),
                    symbol_id: Some(SymbolId(agent_sym.clone())),
                    ours_changeset: ChangesetId("trunk".into()),
                    theirs_changeset: changeset_id.clone(),
                    description: format!("symbol '{}' modified by both trunk and agent", agent_sym),
                    ours_span: None,
                    theirs_span: None,
                    base_span: None,
                });
            }
        }
    }

    if symbol_conflicts.is_empty() {
        MergeCheckResult::Clean
    } else {
        MergeCheckResult::Conflicted(symbol_conflicts)
    }
}

/// Group semantic operations by file path, collecting the symbol names
/// modified in each file.
fn group_ops_by_file(operations: &[SemanticOperation]) -> HashMap<PathBuf, HashSet<String>> {
    let mut map: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for op in operations {
        if let Some(name) = op.symbol_name() {
            map.entry(op.file_path().to_path_buf())
                .or_default()
                .insert(name.to_string());
        }
    }
    map
}
