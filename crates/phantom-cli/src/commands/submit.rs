//! `phantom submit` — submit an agent's work as a changeset.

use anyhow::Context;
use chrono::Utc;
use phantom_core::changeset::SemanticOperation;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
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
