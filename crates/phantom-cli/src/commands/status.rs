//! `phantom status` — show overlays, changesets, and system state.

use phantom_core::event::{Event, EventKind};
use phantom_core::id::AgentId;
use phantom_core::traits::EventStore;
use phantom_events::Projection;

use crate::context::PhantomContext;

pub async fn run() -> anyhow::Result<()> {
    let ctx = PhantomContext::load()?;

    let head = ctx.git.head_oid().map_err(|e| anyhow::anyhow!("{e}"))?;

    let all_events = ctx.events.query_all().map_err(|e| anyhow::anyhow!("{e}"))?;

    let projection = Projection::from_events(&all_events);

    // Header
    let head_short = head.to_hex();
    let head_short = &head_short[..12.min(head_short.len())];
    println!("Trunk HEAD: {head_short}");
    println!();

    // Active overlays
    let active_agents = projection.active_agents();
    if active_agents.is_empty() {
        println!("Active overlays: (none)");
    } else {
        println!("Active overlays:");
        println!("  {:<20} {:<20} PATH", "AGENT", "MODE");
        for agent in &active_agents {
            let overlays_prefix = ctx.phantom_dir.join("overlays");
            let mount = ctx
                .overlays
                .upper_dir(agent)
                .map(|p| {
                    p.strip_prefix(&overlays_prefix)
                        .unwrap_or(p)
                        .display()
                        .to_string()
                })
                .unwrap_or_else(|_| "(not mounted)".into());

            // Determine session mode from projection
            let mode = agent_session_mode(&projection, &all_events, agent);
            println!("  {:<20} {:<20} {}", agent, mode, mount);
        }
    }
    println!();

    // Pending changesets
    let pending = projection.pending_changesets();
    if pending.is_empty() {
        println!("Pending changesets: (none)");
    } else {
        println!("Pending changesets:");
        println!("  {:<12} {:<20} {:<15} FILES", "ID", "AGENT", "STATUS");
        for cs in &pending {
            println!(
                "  {:<12} {:<20} {:<15} {}",
                cs.id,
                cs.agent_id,
                format!("{:?}", cs.status),
                cs.files_touched.len(),
            );
        }
    }
    println!();

    println!("Total events: {}", all_events.len());

    Ok(())
}

/// Determine the display mode for an agent's session.
///
/// Checks the projection's `interactive_session_active` flag and, on Linux,
/// verifies that the recorded PID is still running to detect stale sessions.
fn agent_session_mode(projection: &Projection, events: &[Event], agent: &AgentId) -> String {
    // Find the agent's active changeset
    let changeset = events
        .iter()
        .filter(|e| e.agent_id == *agent)
        .find_map(|e| {
            if matches!(e.kind, EventKind::OverlayCreated { .. }) {
                Some(e.changeset_id.clone())
            } else {
                None
            }
        })
        .and_then(|cs_id| projection.changeset(&cs_id));

    match changeset {
        Some(cs) if cs.interactive_session_active => {
            // Check if the process is still alive (Linux-specific)
            let pid = events
                .iter()
                .rev()
                .filter(|e| e.agent_id == *agent)
                .find_map(|e| match &e.kind {
                    EventKind::InteractiveSessionStarted { pid, .. } => Some(*pid),
                    _ => None,
                });

            if let Some(pid) = pid {
                let proc_path = format!("/proc/{pid}");
                if !std::path::Path::new(&proc_path).exists() {
                    return "interactive (stale)".into();
                }
            }

            "interactive".into()
        }
        _ => "background".into(),
    }
}
