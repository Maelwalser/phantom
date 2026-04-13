//! `phantom materialize` — commit a changeset to trunk.

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::notification::TrunkFileStatus;
use phantom_core::traits::EventStore;
use phantom_events::Projection;
use phantom_orchestrator::live_rebase;
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_orchestrator::ripple::{
    self, RippleChecker,
};

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct MaterializeArgs {
    /// Changeset ID (e.g. "cs-0042") or agent name (e.g. "agent-a")
    pub target: String,

    /// Commit message. Defaults to the agent name if omitted.
    #[arg(short, long)]
    pub message: Option<String>,
}

pub async fn run(args: MaterializeArgs) -> anyhow::Result<()> {
    let mut ctx = PhantomContext::load().await?;

    // Resolve the target changeset: if it looks like a changeset ID use it
    // directly, otherwise treat it as an agent name and find their latest
    // submitted changeset.
    let changeset_id = if args.target.starts_with("cs-") {
        ChangesetId(args.target.clone())
    } else {
        let agent_name = &args.target;
        let agent_id = AgentId(agent_name.clone());

        let all_events = ctx.events.query_all().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let projection = Projection::from_events(&all_events);

        let cs = projection
            .latest_submitted_changeset(&agent_id)
            .with_context(|| format!("no submitted changeset found for agent '{agent_name}'"))?;

        println!("{agent_name} → changeset {}", cs.id);
        cs.id.clone()
    };

    // Resolve commit message: use --message if provided, otherwise default to
    // the agent name from the changeset.
    let commit_message = if let Some(msg) = args.message {
        msg
    } else {
        let all = ctx.events.query_all().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let proj = Projection::from_events(&all);
        let cs = proj
            .changeset(&changeset_id)
            .with_context(|| format!("changeset '{changeset_id}' not found"))?;
        cs.agent_id.0.clone()
    };

    let result = materialize_changeset(&mut ctx, &changeset_id, &commit_message).await?;

    match result {
        MaterializeResult::Success { new_commit } => {
            let short = new_commit.to_hex();
            let short = &short[..12.min(short.len())];
            println!("Materialized {} → commit {short}", changeset_id);
        }
        MaterializeResult::Conflict { details } => {
            eprintln!(
                "Materialization of {} failed with {} conflict(s):\n",
                changeset_id,
                details.len()
            );
            for detail in &details {
                let kind_label = match detail.kind {
                    phantom_core::ConflictKind::BothModifiedSymbol => "both modified",
                    phantom_core::ConflictKind::ModifyDeleteSymbol => "modify/delete",
                    phantom_core::ConflictKind::BothModifiedDependencyVersion => "dependency version",
                    phantom_core::ConflictKind::RawTextConflict => "text conflict",
                    phantom_core::ConflictKind::BinaryFile => "binary file",
                };
                let location = format_conflict_location(detail);
                eprintln!("  {} [{kind_label}]", detail.file.display());
                eprintln!("    {}", detail.description);
                if !location.is_empty() {
                    eprintln!("    {location}");
                }
                eprintln!();
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Format line-number location info for a conflict detail.
fn format_conflict_location(detail: &phantom_core::ConflictDetail) -> String {
    let mut parts = Vec::new();
    if let Some(span) = &detail.ours_span {
        if span.start_line == span.end_line {
            parts.push(format!("ours: line {}", span.start_line));
        } else {
            parts.push(format!("ours: lines {}–{}", span.start_line, span.end_line));
        }
    }
    if let Some(span) = &detail.theirs_span {
        if span.start_line == span.end_line {
            parts.push(format!("theirs: line {}", span.start_line));
        } else {
            parts.push(format!("theirs: lines {}–{}", span.start_line, span.end_line));
        }
    }
    if let Some(span) = &detail.base_span {
        if span.start_line == span.end_line {
            parts.push(format!("base: line {}", span.start_line));
        } else {
            parts.push(format!("base: lines {}–{}", span.start_line, span.end_line));
        }
    }
    parts.join(", ")
}

/// Materialize a changeset to trunk.
///
/// Runs the semantic merge, commits to git, and checks for ripple effects on
/// other active agents. Returns the [`MaterializeResult`].
pub async fn materialize_changeset(
    ctx: &mut PhantomContext,
    changeset_id: &ChangesetId,
    message: &str,
) -> anyhow::Result<MaterializeResult> {
    let all_events = ctx.events.query_all().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let projection = Projection::from_events(&all_events);

    let changeset = projection
        .changeset(changeset_id)
        .with_context(|| format!("changeset '{changeset_id}' not found"))?
        .clone();

    let upper_dir = ctx
        .overlays
        .upper_dir(&changeset.agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .to_path_buf();

    let materializer = Materializer::new(
        phantom_orchestrator::git::GitOps::open(&ctx.repo_root)
            .context("failed to open git repo for materialization")?,
    );

    let result = materializer
        .materialize(&changeset, &upper_dir, &ctx.events, &ctx.semantic, message)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Clear the agent's upper layer so reads fall through to the updated trunk.
    if let MaterializeResult::Success { .. } = &result
        && let Err(e) = ctx.overlays.clear_overlay(&changeset.agent_id)
    {
        eprintln!(
            "warning: failed to clear upper layer for {}: {e}",
            changeset.agent_id
        );
    }

    // Run ripple check and live rebase on success.
    if let MaterializeResult::Success { .. } = &result
        && let Ok(head) = materializer.git().head_oid()
        && let Ok(changed_files) = materializer
            .git()
            .changed_files(&changeset.base_commit, &head)
    {
        let active: Vec<(AgentId, Vec<std::path::PathBuf>)> = projection
            .active_agents()
            .into_iter()
            .filter(|a| *a != changeset.agent_id)
            .filter_map(|a| {
                let agent_cs = all_events
                    .iter()
                    .filter(|e| e.agent_id == a)
                    .find_map(|e| match &e.kind {
                        EventKind::OverlayCreated { .. } => Some(e.changeset_id.clone()),
                        _ => None,
                    });
                agent_cs.and_then(|cs_id| {
                    projection
                        .changeset(&cs_id)
                        .map(|cs| (a.clone(), cs.files_touched.clone()))
                })
            })
            .collect();

        let affected = RippleChecker::check_ripple(&changed_files, &active);
        if !affected.is_empty() {
            println!("Ripple: the following agents may be affected:");
            for (agent_id, files) in &affected {
                let upper = ctx.overlays.upper_dir(agent_id).ok().map(|p| p.to_path_buf());
                let Some(upper_path) = upper.as_deref() else {
                    continue;
                };

                let classified = ripple::classify_trunk_changes(files, upper_path);
                let shadowed_files: Vec<std::path::PathBuf> = classified
                    .iter()
                    .filter(|(_, s)| *s == TrunkFileStatus::Shadowed)
                    .map(|(p, _)| p.clone())
                    .collect();

                if shadowed_files.is_empty() {
                    // No shadowed files — just write notification and update base.
                    let notif = ripple::build_notification(head, classified);
                    if let Err(e) = ripple::write_trunk_notification(&ctx.phantom_dir, agent_id, &notif) {
                        eprintln!("warning: failed to write notification for {agent_id}: {e}");
                    }
                    if let Err(e) = live_rebase::write_current_base(&ctx.phantom_dir, agent_id, &head) {
                        eprintln!("warning: failed to update current_base for {agent_id}: {e}");
                    }
                    println!("  {agent_id}: {} file(s)", files.len());
                    continue;
                }

                // Determine the agent's current base commit.
                let old_base = live_rebase::read_current_base(&ctx.phantom_dir, agent_id)
                    .ok()
                    .flatten()
                    .unwrap_or(changeset.base_commit);

                // Attempt live rebase on shadowed files.
                match live_rebase::rebase_agent(
                    materializer.git(),
                    &ctx.semantic,
                    agent_id,
                    &old_base,
                    &head,
                    upper_path,
                    &shadowed_files,
                ) {
                    Ok(rebase_result) => {
                        // Update current_base to new trunk head.
                        if let Err(e) = live_rebase::write_current_base(&ctx.phantom_dir, agent_id, &head) {
                            eprintln!("warning: failed to update current_base for {agent_id}: {e}");
                        }

                        // Build enriched notification.
                        let enriched: Vec<(std::path::PathBuf, TrunkFileStatus)> = classified
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
                        if let Err(e) = ripple::write_trunk_notification(&ctx.phantom_dir, agent_id, &notif) {
                            eprintln!("warning: failed to write notification for {agent_id}: {e}");
                        }

                        // Emit audit event.
                        let event = Event {
                            id: EventId(0),
                            timestamp: Utc::now(),
                            changeset_id: changeset_id.clone(),
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
                        if let Err(e) = ctx.events.append(event).await {
                            eprintln!("warning: failed to record live rebase event for {agent_id}: {e}");
                        }

                        let merged = rebase_result.merged.len();
                        let conflicted = rebase_result.conflicted.len();
                        println!(
                            "  {agent_id}: {} file(s) ({merged} merged, {conflicted} conflicted)",
                            files.len()
                        );
                    }
                    Err(e) => {
                        // Rebase failed — fall back to notification-only (old behavior).
                        eprintln!("warning: live rebase failed for {agent_id}: {e}");
                        let notif = ripple::build_notification(head, classified);
                        if let Err(e) = ripple::write_trunk_notification(&ctx.phantom_dir, agent_id, &notif) {
                            eprintln!("warning: failed to write notification for {agent_id}: {e}");
                        }
                        println!(
                            "  {agent_id}: {} file(s) ({} shadowed, rebase failed)",
                            files.len(),
                            shadowed_files.len()
                        );
                    }
                }
            }
        }
    }

    Ok(result)
}
