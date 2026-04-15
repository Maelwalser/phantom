//! `phantom rollback` — drop a changeset and revert its changes from trunk.
//!
//! When invoked without a changeset ID, presents an interactive menu of
//! materialized changesets (newest first) and rolls back all changesets
//! after the selected checkpoint.

use anyhow::Context;
use dialoguer::Select;
use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::event::EventKind;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::ReplayEngine;
use phantom_events::projection::Projection;
use phantom_events::snapshot::SnapshotManager;
use phantom_events::store::SqliteEventStore;
use phantom_git::GitOps;

use super::ui;
use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct RollbackArgs {
    /// Changeset ID (e.g. "cs-0040") or agent name. Omit for interactive selection.
    pub target: Option<String>,
}

pub async fn run(args: RollbackArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;

    match args.target {
        None => run_interactive(&ctx, &events).await?,
        Some(ref target) if target.starts_with("cs-") => {
            rollback_single(&ctx, &events, &ChangesetId(target.clone())).await?
        }
        Some(agent_name) => {
            run_interactive_for_agent(&ctx, &events, &AgentId(agent_name)).await?
        }
    }

    Ok(())
}

/// Roll back a single changeset: mark events as dropped, revert the git commit,
/// and report downstream changesets that may need re-dispatch.
async fn rollback_single(
    ctx: &PhantomContext,
    events: &SqliteEventStore,
    changeset_id: &ChangesetId,
) -> anyhow::Result<()> {
    let cs_events = events.query_by_changeset(changeset_id).await?;

    let materialized_commit: Option<GitOid> = cs_events.iter().find_map(|e| {
        if let EventKind::ChangesetMaterialized { new_commit } = &e.kind {
            Some(*new_commit)
        } else {
            None
        }
    });

    let dropped = events.mark_dropped(changeset_id).await?;

    println!(
        "  {} Dropped {dropped} event(s) for changeset {}.",
        console::style("↺").yellow(),
        console::style(&changeset_id.to_string()).bold()
    );

    match materialized_commit {
        None => {
            println!(
                "  {} Changeset was not materialized — no git changes to revert.",
                console::style("·").dim()
            );
        }
        Some(commit_oid) => {
            let git =
                GitOps::open(&ctx.repo_root).context("failed to open git repo for rollback")?;

            let message = format!("phantom: rollback {changeset_id}");
            match git.revert_commit_oid(&commit_oid, &message) {
                Ok(revert_oid) => {
                    let short = revert_oid.to_hex();
                    let short = &short[..12.min(short.len())];
                    println!(
                        "  {} Reverted commit → {}",
                        console::style("✓").green(),
                        console::style(short).cyan()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "  {} git revert failed: {e}",
                        console::style("⚠").yellow()
                    );
                    eprintln!("    The rolled-back changes may have been modified by later commits.");
                    eprintln!("    Manual resolution with `git revert` may be needed.");
                }
            }
        }
    }

    let replay = ReplayEngine::new(events);
    let downstream = replay.changesets_after(changeset_id).await?;

    if downstream.is_empty() {
        println!(
            "  {}",
            console::style("No downstream changesets affected.").dim()
        );
    } else {
        println!(
            "  {} Downstream changesets requiring re-task:",
            console::style("⚠").yellow()
        );
        for cs in &downstream {
            println!("    {}", console::style(cs.to_string()).bold());
        }
    }

    Ok(())
}

/// Present an interactive menu of materialized changesets and roll back
/// everything after the selected checkpoint.
async fn run_interactive(ctx: &PhantomContext, events: &SqliteEventStore) -> anyhow::Result<()> {
    let replay = ReplayEngine::new(events);
    let materialized = replay.materialized_changesets().await?;

    if materialized.is_empty() {
        println!("No materialized changesets to roll back.");
        return Ok(());
    }

    // Build projection for display metadata (task, agent, timestamp).
    let projection = SnapshotManager::new(events).build_projection().await?;

    // Reverse to newest-first for the menu.
    let materialized_rev: Vec<&ChangesetId> = materialized.iter().rev().collect();

    let display_items: Vec<String> = materialized_rev
        .iter()
        .map(|id| match projection.changeset(id) {
            Some(cs) => format_menu_item(id, cs),
            None => format!("{}", id),
        })
        .collect();

    let selection = Select::new()
        .with_prompt("Select a checkpoint to restore (all newer changesets will be rolled back)")
        .items(&display_items)
        .default(0)
        .interact_opt()?;

    let Some(idx) = selection else {
        println!("Cancelled.");
        return Ok(());
    };

    if idx == 0 {
        println!("This is already the latest state. Nothing to roll back.");
        return Ok(());
    }

    // Roll back changesets at indices 0..idx (newest first).
    let to_rollback = &materialized_rev[..idx];
    println!(
        "\n  {} Rolling back {} changeset(s)...\n",
        console::style("↺").yellow(),
        to_rollback.len()
    );

    for cs_id in to_rollback {
        rollback_single(ctx, events, cs_id).await?;
        println!();
    }

    println!("Rolled back to checkpoint: {}", materialized_rev[idx]);

    Ok(())
}

/// Present an interactive menu of rollback-eligible changesets for a specific
/// agent and roll back the selected one.
async fn run_interactive_for_agent(
    ctx: &PhantomContext,
    events: &SqliteEventStore,
    agent_id: &AgentId,
) -> anyhow::Result<()> {
    let agent_events = events.query_by_agent(agent_id).await?;

    if agent_events.is_empty() {
        anyhow::bail!("no events found for agent '{agent_id}'");
    }

    let projection = Projection::from_events(&agent_events);

    // Filter to Submitted or Materialized changesets (rollback-eligible).
    let mut eligible: Vec<&Changeset> = projection
        .changesets_for_agent(agent_id)
        .into_iter()
        .filter(|cs| {
            matches!(
                cs.status,
                ChangesetStatus::Submitted
            )
        })
        .collect();
    // Newest first (changesets_for_agent already sorts this way, but be explicit).
    eligible.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if eligible.is_empty() {
        println!("No rollback-eligible changesets for agent '{agent_id}'.");
        return Ok(());
    }

    let display_items: Vec<String> = eligible
        .iter()
        .map(|cs| format_menu_item(&cs.id, cs))
        .collect();

    let selection = Select::new()
        .with_prompt(format!(
            "Select changeset to roll back for agent '{agent_id}'"
        ))
        .items(&display_items)
        .default(0)
        .interact_opt()?;

    let Some(idx) = selection else {
        println!("Cancelled.");
        return Ok(());
    };

    let cs_id = &eligible[idx].id;
    rollback_single(ctx, events, cs_id).await?;

    Ok(())
}

fn format_menu_item(id: &ChangesetId, cs: &Changeset) -> String {
    let task_display = if cs.task.len() > 50 {
        format!("{}...", &cs.task[..47])
    } else {
        cs.task.clone()
    };
    let age = ui::relative_time(cs.created_at);
    format!(
        "{:<10}  {:<14}  {:50}  ({})",
        id.0, cs.agent_id.0, task_display, age
    )
}
