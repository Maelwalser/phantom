//! `phantom rollback` — drop a changeset and identify downstream impacts.

use phantom_core::event::EventKind;
use phantom_core::id::ChangesetId;
use phantom_core::traits::EventStore;
use phantom_events::ReplayEngine;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct RollbackArgs {
    /// Changeset ID to roll back (e.g. "cs-0040")
    #[arg(long)]
    pub changeset: String,
}

pub async fn run(args: RollbackArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::load()?;

    let changeset_id = ChangesetId(args.changeset.clone());

    // Find the commit OID before this changeset's materialization
    let cs_events = ctx
        .events
        .query_by_changeset(&changeset_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let materialized_event = cs_events
        .iter()
        .find(|e| matches!(e.kind, EventKind::ChangesetMaterialized { .. }));

    if materialized_event.is_none() {
        // Changeset was never materialized — just mark events as dropped
        let dropped = ctx
            .events
            .mark_dropped(&changeset_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        println!(
            "Dropped {dropped} event(s) for changeset {}.",
            args.changeset
        );
        println!("Changeset was not materialized — no git changes to revert.");
        return Ok(());
    }

    // Mark events as dropped
    let dropped = ctx
        .events
        .mark_dropped(&changeset_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "Dropped {dropped} event(s) for changeset {}.",
        args.changeset
    );

    // Find downstream changesets that were materialized after this one
    let replay = ReplayEngine::new(&ctx.events);
    let downstream = replay
        .changesets_after(&changeset_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if downstream.is_empty() {
        println!("No downstream changesets affected.");
    } else {
        println!("Downstream changesets requiring re-dispatch:");
        for cs in &downstream {
            println!("  {cs}");
        }
    }

    println!();
    println!("Note: Git history has not been modified. Use `git reset` manually if needed.");
    println!("Re-dispatch affected agents to rebuild on the updated trunk.");

    Ok(())
}
