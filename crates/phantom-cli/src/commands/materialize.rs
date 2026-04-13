//! `phantom materialize` — commit a changeset to trunk.

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_events::Projection;
use phantom_orchestrator::materialization_service::{
    self, ActiveOverlay, MaterializeOutput,
};
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_core::event::EventKind;

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
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    // Resolve the target changeset: if it looks like a changeset ID use it
    // directly, otherwise treat it as an agent name and find their latest
    // submitted changeset.
    let changeset_id = if args.target.starts_with("cs-") {
        ChangesetId(args.target.clone())
    } else {
        let agent_name = &args.target;
        let agent_id = AgentId(agent_name.clone());

        let all_events = events.query_all().await?;
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
        let all = events.query_all().await?;
        let proj = Projection::from_events(&all);
        let cs = proj
            .changeset(&changeset_id)
            .with_context(|| format!("changeset '{changeset_id}' not found"))?;
        cs.agent_id.0.clone()
    };

    let output = materialize_changeset(&ctx, &events, &mut overlays, &changeset_id, &commit_message).await?;

    match output.result {
        MaterializeResult::Success { new_commit } => {
            let short = new_commit.to_hex();
            let short = &short[..12.min(short.len())];
            println!("Materialized {} → commit {short}", changeset_id);

            if !output.ripple_effects.is_empty() {
                println!("Ripple: the following agents may be affected:");
                for effect in &output.ripple_effects {
                    if effect.merged_count > 0 || effect.conflicted_count > 0 {
                        println!(
                            "  {}: {} file(s) ({} merged, {} conflicted)",
                            effect.agent_id,
                            effect.files.len(),
                            effect.merged_count,
                            effect.conflicted_count,
                        );
                    } else {
                        println!("  {}: {} file(s)", effect.agent_id, effect.files.len());
                    }
                }
            }
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
/// other active agents. Returns a [`MaterializeOutput`] with the result and
/// ripple effects.
pub async fn materialize_changeset(
    ctx: &PhantomContext,
    events: &dyn EventStore,
    overlays: &mut phantom_overlay::OverlayManager,
    changeset_id: &ChangesetId,
    message: &str,
) -> anyhow::Result<MaterializeOutput> {
    let all_events = events.query_all().await?;
    let projection = Projection::from_events(&all_events);

    let changeset = projection
        .changeset(changeset_id)
        .with_context(|| format!("changeset '{changeset_id}' not found"))?
        .clone();

    let upper_dir = overlays
        .upper_dir(&changeset.agent_id)?
        .to_path_buf();

    let materializer = Materializer::new(
        phantom_orchestrator::git::GitOps::open(&ctx.repo_root)
            .context("failed to open git repo for materialization")?,
    );
    let analyzer = ctx.semantic();

    // Build the list of active overlays for ripple checking.
    let active_overlays: Vec<ActiveOverlay> = projection
        .active_agents()
        .into_iter()
        .filter(|a| *a != changeset.agent_id)
        .filter_map(|a| {
            let agent_cs = all_events
                .iter()
                .filter(|e| e.agent_id == a)
                .find_map(|e| match &e.kind {
                    EventKind::TaskCreated { .. } => Some(e.changeset_id.clone()),
                    _ => None,
                });
            let cs_data = agent_cs
                .and_then(|cs_id| projection.changeset(&cs_id).cloned());
            let agent_upper = overlays.upper_dir(&a).ok().map(|p| p.to_path_buf());
            match (cs_data, agent_upper) {
                (Some(cs), Some(upper)) => Some(ActiveOverlay {
                    agent_id: a.clone(),
                    files_touched: cs.files_touched.clone(),
                    upper_dir: upper,
                }),
                _ => None,
            }
        })
        .collect();

    // Prepare the overlay-clear callback.
    let agent_id_for_clear = changeset.agent_id.clone();
    let mut clear_fn = || {
        overlays
            .clear_overlay(&agent_id_for_clear)
            .map_err(|e| e.to_string())
    };

    let output = materialization_service::materialize_and_ripple(
        &changeset,
        &upper_dir,
        events,
        &analyzer,
        &materializer,
        &ctx.phantom_dir,
        &active_overlays,
        message,
        &mut clear_fn,
    )
    .await?;

    Ok(output)
}
