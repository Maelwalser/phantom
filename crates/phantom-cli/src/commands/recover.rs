//! `ph recover` — reconcile orphan pre-commit fences after a crash.
//!
//! Scans the event log for `ChangesetMaterializationStarted` events that
//! have no subsequent terminal event and, for each one, decides whether the
//! git commit landed. Commits found on trunk get a reconstructed
//! `ChangesetMaterialized` event; missing commits get a `ChangesetDropped`
//! terminal so the projection stops treating the changeset as in-flight.
//!
//! Safe to run against a healthy repo — the scan is cheap and emits nothing
//! when there are no orphans.

use phantom_orchestrator::recovery::{self, RecoveryReport};

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct RecoverArgs {}

pub async fn run(_args: RecoverArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let git = ctx.open_git()?;

    let report = recovery::reconcile_orphan_fences(&git, &events).await?;

    print_report(&report);
    Ok(())
}

fn print_report(report: &RecoveryReport) {
    if report.total() == 0 {
        println!(
            "  {} No orphan fence events found. Trunk and event log are consistent.",
            console::style("✓").green()
        );
        return;
    }

    if !report.reconstructed.is_empty() {
        println!(
            "  {} Reconstructed {} missing `ChangesetMaterialized` event(s):",
            console::style("↻").green(),
            report.reconstructed.len()
        );
        for r in &report.reconstructed {
            println!(
                "    {} {} → commit {}",
                console::style("·").dim(),
                console::style(&r.changeset_id.to_string()).bold(),
                console::style(short_hex(&r.new_commit.to_hex())).cyan()
            );
        }
    }

    if !report.aborted.is_empty() {
        println!(
            "  {} Marked {} fence(s) aborted (no matching commit on trunk):",
            console::style("✗").yellow(),
            report.aborted.len()
        );
        for a in &report.aborted {
            println!(
                "    {} {} (fence event {})",
                console::style("·").dim(),
                console::style(&a.changeset_id.to_string()).bold(),
                a.fence_event_id.0
            );
        }
    }
}

fn short_hex(hex: &str) -> &str {
    &hex[..12.min(hex.len())]
}
