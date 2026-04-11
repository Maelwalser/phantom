//! `phantom materialize` — commit a changeset to trunk.

use anyhow::Context;
use phantom_core::event::EventKind;
use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_events::Projection;
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_orchestrator::ripple::RippleChecker;

use crate::context::PhantomContext;

#[derive(clap::Args)]
#[command(group(
    clap::ArgGroup::new("target")
        .required(true)
        .args(["changeset", "agent"]),
))]
pub struct MaterializeArgs {
    /// Changeset ID to materialize (e.g. "cs-0042")
    #[arg(long)]
    pub changeset: Option<String>,

    /// Agent whose latest submitted changeset to materialize
    #[arg(long)]
    pub agent: Option<String>,
}

pub async fn run(args: MaterializeArgs) -> anyhow::Result<()> {
    let mut ctx = PhantomContext::load()?;

    // Resolve the target changeset: either directly by ID or via agent lookup.
    let changeset_id = if let Some(cs) = &args.changeset {
        ChangesetId(cs.clone())
    } else {
        // Safe to unwrap: ArgGroup guarantees exactly one of changeset/agent is set.
        let agent_name = args.agent.as_ref().unwrap();
        let agent_id = AgentId(agent_name.clone());

        let all_events = ctx.events.query_all().map_err(|e| anyhow::anyhow!("{e}"))?;
        let projection = Projection::from_events(&all_events);

        let cs = projection
            .latest_submitted_changeset(&agent_id)
            .with_context(|| {
                format!("no submitted changeset found for agent '{agent_name}'")
            })?;

        println!("Resolved agent '{agent_name}' → changeset '{}'", cs.id);
        cs.id.clone()
    };

    let result = materialize_changeset(&mut ctx, &changeset_id)?;

    match result {
        MaterializeResult::Success { new_commit } => {
            let short = new_commit.to_hex();
            let short = &short[..12.min(short.len())];
            println!("Materialized {} → commit {short}", changeset_id);
        }
        MaterializeResult::Conflict { details } => {
            eprintln!(
                "Materialization of {} failed with {} conflict(s):",
                changeset_id,
                details.len()
            );
            for detail in &details {
                eprintln!(
                    "  [{:?}] {} — {}",
                    detail.kind,
                    detail.file.display(),
                    detail.description
                );
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Materialize a changeset to trunk.
///
/// Runs the semantic merge, commits to git, and checks for ripple effects on
/// other active agents. Returns the [`MaterializeResult`].
pub fn materialize_changeset(
    ctx: &mut PhantomContext,
    changeset_id: &ChangesetId,
) -> anyhow::Result<MaterializeResult> {
    let all_events = ctx.events.query_all().map_err(|e| anyhow::anyhow!("{e}"))?;
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
        .materialize(&changeset, &upper_dir, &ctx.events, &ctx.semantic)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Clear the agent's upper layer so reads fall through to the updated trunk.
    if let MaterializeResult::Success { .. } = &result {
        if let Err(e) = ctx.overlays.clear_overlay(&changeset.agent_id) {
            eprintln!(
                "warning: failed to clear upper layer for {}: {e}",
                changeset.agent_id
            );
        }
    }

    // Run ripple check on success
    if let MaterializeResult::Success { .. } = &result {
        if let Ok(head) = materializer.git().head_oid() {
            if let Ok(changed_files) =
                materializer.git().changed_files(&changeset.base_commit, &head)
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
                                EventKind::OverlayCreated { .. } => {
                                    Some(e.changeset_id.clone())
                                }
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
                    for (agent, files) in &affected {
                        println!("  {agent}: {} file(s)", files.len());
                    }
                }
            }
        }
    }

    Ok(result)
}
