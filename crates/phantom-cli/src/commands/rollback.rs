//! `phantom rollback` — drop a changeset and revert its changes from trunk.

use anyhow::Context;
use phantom_core::event::EventKind;
use phantom_core::id::{ChangesetId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::ReplayEngine;
use phantom_orchestrator::git::GitOps;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct RollbackArgs {
    /// Changeset ID to roll back (e.g. "cs-0040")
    pub changeset: String,
}

pub async fn run(args: RollbackArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::load().await?;

    let changeset_id = ChangesetId(args.changeset.clone());

    // Find the commit OID from this changeset's materialization
    let cs_events = ctx
        .events
        .query_by_changeset(&changeset_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let materialized_commit: Option<GitOid> = cs_events.iter().find_map(|e| {
        if let EventKind::ChangesetMaterialized { new_commit } = &e.kind {
            Some(*new_commit)
        } else {
            None
        }
    });

    // Mark events as dropped
    let dropped = ctx
        .events
        .mark_dropped(&changeset_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "Dropped {dropped} event(s) for changeset {}.",
        args.changeset
    );

    match materialized_commit {
        None => {
            println!("Changeset was not materialized — no git changes to revert.");
        }
        Some(commit_oid) => {
            // Actually revert the git commit
            let git =
                GitOps::open(&ctx.repo_root).context("failed to open git repo for rollback")?;

            let message = format!("phantom: rollback {}", args.changeset);
            match git.revert_commit_oid(&commit_oid, &message) {
                Ok(revert_oid) => {
                    let short = revert_oid.to_hex();
                    let short = &short[..12.min(short.len())];
                    println!("Reverted commit → {short}");
                }
                Err(e) => {
                    eprintln!("Warning: git revert failed: {e}");
                    eprintln!("The rolled-back changes may have been modified by later commits.");
                    eprintln!("Manual resolution with `git revert` may be needed.");
                }
            }
        }
    }

    // Find downstream changesets that were materialized after this one
    let replay = ReplayEngine::new(&ctx.events);
    let downstream = replay
        .changesets_after(&changeset_id)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if downstream.is_empty() {
        println!("No downstream changesets affected.");
    } else {
        println!("Downstream changesets requiring re-dispatch:");
        for cs in &downstream {
            println!("  {cs}");
        }
    }

    Ok(())
}
