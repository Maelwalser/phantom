//! `phantom materialize` — commit a changeset to trunk.

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::EventStore;
use phantom_events::Projection;
use phantom_orchestrator::materializer::{MaterializeResult, Materializer};
use phantom_orchestrator::ripple::RippleChecker;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct MaterializeArgs {
    /// Changeset ID to materialize (e.g. "cs-0042")
    #[arg(long)]
    pub changeset: String,
}

pub async fn run(args: MaterializeArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::load()?;

    let changeset_id = ChangesetId(args.changeset.clone());

    let all_events = ctx.events.query_all().map_err(|e| anyhow::anyhow!("{e}"))?;

    let projection = Projection::from_events(&all_events);

    let changeset = projection
        .changeset(&changeset_id)
        .with_context(|| format!("changeset '{}' not found", args.changeset))?
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

    match result {
        MaterializeResult::Success { new_commit } => {
            let short = new_commit.to_hex();
            let short = &short[..12.min(short.len())];
            println!("Materialized {} → commit {short}", args.changeset);

            // Run ripple check
            let head = materializer
                .git()
                .head_oid()
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            if let Ok(changed_files) = materializer
                .git()
                .changed_files(&changeset.base_commit, &head)
            {
                let active: Vec<(AgentId, Vec<std::path::PathBuf>)> = projection
                    .active_agents()
                    .into_iter()
                    .filter(|a| *a != changeset.agent_id)
                    .map(|a| {
                        let files = projection
                            .changeset(&changeset_id)
                            .map(|cs| cs.files_touched.clone())
                            .unwrap_or_default();
                        (a, files)
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
        MaterializeResult::Conflict { details } => {
            eprintln!(
                "Materialization of {} failed with {} conflict(s):",
                args.changeset,
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
