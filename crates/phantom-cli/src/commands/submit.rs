//! `phantom submit` — submit an agent's work as a changeset.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Context;
use chrono::Utc;
use phantom_core::changeset::SemanticOperation;
use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::event::{Event, EventKind, MergeCheckResult};
use phantom_core::id::{AgentId, ChangesetId, EventId, SymbolId};
use phantom_core::traits::{EventStore, SemanticAnalyzer};

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct SubmitArgs {
    /// Agent identifier whose work to submit
    pub agent: String,
}

pub async fn run(args: SubmitArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::load()?;
    let agent_id = AgentId(args.agent.clone());

    match submit_agent(&ctx, &agent_id)? {
        Some(changeset_id) => {
            println!("Changeset {changeset_id} submitted.");
        }
        None => {
            println!("No modified files found for agent '{}'.", args.agent);
        }
    }

    Ok(())
}

/// Submit an agent's overlay work as a changeset.
///
/// Returns `Some(changeset_id)` if changes were found and submitted,
/// or `None` if the overlay has no modifications.
pub fn submit_agent(
    ctx: &PhantomContext,
    agent_id: &AgentId,
) -> anyhow::Result<Option<ChangesetId>> {
    let layer = ctx
        .overlays
        .get_layer(agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let modified = layer.modified_files().map_err(|e| anyhow::anyhow!("{e}"))?;

    if modified.is_empty() {
        return Ok(None);
    }

    // Find the changeset ID and base commit for this agent from events
    let events = ctx
        .events
        .query_by_agent(agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let (changeset_id, base_commit) = events
        .iter()
        .rev()
        .find_map(|e| {
            if let EventKind::OverlayCreated { base_commit, .. } = &e.kind {
                Some((e.changeset_id.clone(), *base_commit))
            } else {
                None
            }
        })
        .context("no overlay found for this agent — was it dispatched?")?;

    let upper_dir = ctx
        .overlays
        .upper_dir(agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut all_ops: Vec<SemanticOperation> = Vec::new();
    let mut additions = 0u32;
    let mut modifications = 0u32;
    let mut deletions = 0u32;

    for file in &modified {
        let agent_content = std::fs::read(upper_dir.join(file))
            .with_context(|| format!("failed to read agent file: {}", file.display()))?;

        let base_content = ctx.git.read_file_at_commit(&base_commit, file);

        let ops = match base_content {
            Ok(base) => {
                let base_symbols = ctx
                    .semantic
                    .extract_symbols(file, &base)
                    .unwrap_or_default();
                let current_symbols = ctx
                    .semantic
                    .extract_symbols(file, &agent_content)
                    .unwrap_or_default();
                ctx.semantic.diff_symbols(&base_symbols, &current_symbols)
            }
            Err(_) => {
                // New file — all symbols are additions
                let symbols = ctx
                    .semantic
                    .extract_symbols(file, &agent_content)
                    .unwrap_or_default();
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

    // If semantic analysis yielded no structured ops, record raw diffs
    if all_ops.is_empty() && !modified.is_empty() {
        for file in &modified {
            all_ops.push(SemanticOperation::RawDiff {
                path: file.clone(),
                patch: String::new(),
            });
            modifications += 1;
        }
    }

    // Early conflict pre-check: compare the agent's operations against
    // current trunk. If trunk has advanced since the agent's base commit,
    // detect symbol-level overlaps and warn about potential conflicts.
    let merge_check = precheck_conflicts(ctx, &base_commit, &all_ops, &changeset_id);

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::ChangesetSubmitted {
            operations: all_ops,
        },
    };
    ctx.events
        .append(event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

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
    ctx.events
        .append(check_event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Remove stale trunk notification — the agent is submitting, so any prior
    // notification is no longer relevant.
    phantom_orchestrator::ripple::remove_trunk_notification(&ctx.phantom_dir, agent_id);

    println!(
        "  {additions} additions, {modifications} modifications, {deletions} deletions across {} file(s)",
        modified.len()
    );
    for f in &modified {
        println!("    {}", f.display());
    }

    Ok(Some(changeset_id))
}

/// Compare the agent's submitted operations against the current trunk state
/// to detect potential symbol-level conflicts early.
///
/// If trunk has advanced since the agent's base commit, this checks whether
/// any symbols the agent modified were also modified on trunk. Returns a
/// [`MergeCheckResult`] that is recorded as an audit event.
fn precheck_conflicts(
    ctx: &PhantomContext,
    base_commit: &phantom_core::id::GitOid,
    operations: &[SemanticOperation],
    changeset_id: &ChangesetId,
) -> MergeCheckResult {
    let head = match ctx.git.head_oid() {
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

        let base = match ctx.git.read_file_at_commit(base_commit, file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let trunk = match ctx.git.read_file_at_commit(&head, file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if base == trunk {
            continue;
        }

        let base_symbols = ctx
            .semantic
            .extract_symbols(file, &base)
            .unwrap_or_default();
        let trunk_symbols = ctx
            .semantic
            .extract_symbols(file, &trunk)
            .unwrap_or_default();
        let trunk_ops = ctx.semantic.diff_symbols(&base_symbols, &trunk_symbols);
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
                    description: format!(
                        "symbol '{}' modified by both trunk and agent",
                        agent_sym
                    ),
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
        println!("  Warning: potential conflicts with current trunk:");
        for conflict in &symbol_conflicts {
            println!(
                "    {} — symbol '{}' modified on both sides",
                conflict.file.display(),
                conflict
                    .symbol_id
                    .as_ref()
                    .map(|s| s.0.as_str())
                    .unwrap_or("?")
            );
        }
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
