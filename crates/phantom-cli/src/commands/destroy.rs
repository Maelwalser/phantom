//! `phantom destroy` — tear down an agent's overlay.
//!
//! If a FUSE daemon is running for the agent, it is unmounted (via
//! `fusermount3 -u`) before the overlay directories are removed.

use std::time::Duration;

use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, EventId};
use phantom_core::traits::EventStore;
use tracing::info;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct DestroyArgs {
    /// Agent identifier whose overlay to destroy
    pub agent: String,
}

pub async fn run(args: DestroyArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events_store = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    let agent_id = AgentId(args.agent.clone());

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
        "  {} Agent '{}' overlay destroyed.",
        console::style("✗").yellow(),
        console::style(&args.agent).bold()
    );
    Ok(())
}

/// Attempt to cleanly unmount a FUSE overlay and kill its daemon.
fn unmount_fuse(phantom_dir: &std::path::Path, agent: &str) {
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let pid_file = overlay_root.join("fuse.pid");
    let mount_point = overlay_root.join("mount");

    if !pid_file.exists() {
        return;
    }

    // Try fusermount3 first (clean unmount)
    let unmount_ok = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(&mount_point)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if unmount_ok {
        info!(agent, "FUSE unmounted cleanly");
    } else {
        // Fallback: kill the daemon process
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = pid_str.trim().parse::<i32>()
        {
            // SAFETY: Sending SIGTERM to a process has no memory-safety
            // implications. The PID comes from a file we wrote.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(200));
            info!(agent, pid, "killed FUSE daemon");
        }
    }

    let _ = std::fs::remove_file(&pid_file);
}
