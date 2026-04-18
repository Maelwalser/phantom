//! `phantom remove` — tear down an agent's overlay.
//!
//! If a FUSE daemon is running for the agent, it is unmounted (via
//! `fusermount3 -u`) before the overlay directories are removed.

use std::time::Duration;

use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;
use tracing::{info, warn};

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct RemoveArgs {
    /// Agent identifier whose overlay to remove
    pub agent: String,
}

pub async fn run(args: RemoveArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events_store = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    let agent_id = crate::services::validate::agent_id(&args.agent)?;

    // Find the changeset ID for this agent
    let events = events_store.query_by_agent(&agent_id).await?;

    let changeset_id = events
        .iter()
        .rev()
        .find_map(|e| {
            if matches!(e.kind, EventKind::TaskCreated { .. }) {
                Some(e.changeset_id.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no overlay found for agent '{}' — was it tasked?",
                args.agent
            )
        })?;

    // Unmount FUSE before removing directories
    unmount_fuse(&ctx.phantom_dir, &args.agent);

    overlays.destroy_overlay(&agent_id)?;

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id,
        agent_id,
        causal_parent: None,
        kind: EventKind::TaskDestroyed,
    };
    events_store.append(event).await?;

    println!(
        "  {} Agent '{}' overlay removed.",
        console::style("✗").yellow(),
        console::style(&args.agent).bold()
    );
    Ok(())
}

/// Attempt to cleanly unmount a FUSE overlay and kill its daemon.
pub(crate) fn unmount_fuse(phantom_dir: &std::path::Path, agent: &str) {
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let pid_file = overlay_root.join("fuse.pid");
    let mount_point = overlay_root.join("mount");

    if !pid_file.exists() {
        return;
    }

    // Try fusermount3 first (clean unmount)
    if crate::fs::fuse::unmount(&mount_point) {
        info!(agent, "FUSE unmounted cleanly");
    } else {
        // Fallback: kill the daemon process (with PID reuse protection).
        if let Some(record) = crate::pid_guard::read_pid_file(&pid_file)
            && crate::pid_guard::kill_process(&record, libc::SIGTERM)
        {
            std::thread::sleep(Duration::from_millis(200));
            info!(agent, pid = record.pid, "killed FUSE daemon");
        }
    }

    let _ = std::fs::remove_file(&pid_file);
}

/// Best-effort overlay cleanup after successful materialization.
///
/// Unmounts FUSE, removes overlay directories, and emits a `TaskDestroyed`
/// event. Errors are logged but not propagated — the submission already
/// succeeded, so the overlay is just stale at this point.
pub(crate) async fn remove_agent_overlay(
    ctx: &PhantomContext,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
) {
    unmount_fuse(&ctx.phantom_dir, &agent_id.0);

    let mut overlays = match ctx.open_overlays_restored() {
        Ok(o) => o,
        Err(e) => {
            warn!(agent = %agent_id, error = %e, "failed to open overlays for post-submit cleanup");
            return;
        }
    };

    if let Err(e) = overlays.destroy_overlay(agent_id) {
        warn!(agent = %agent_id, error = %e, "failed to remove overlay after successful submit");
        return;
    }

    let events = match ctx.open_events().await {
        Ok(e) => e,
        Err(e) => {
            warn!(agent = %agent_id, error = %e, "failed to open event store for TaskDestroyed event");
            return;
        }
    };

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent: None,
        kind: EventKind::TaskDestroyed,
    };

    if let Err(e) = events.append(event).await {
        warn!(agent = %agent_id, error = %e, "failed to emit TaskDestroyed event");
    }
}
