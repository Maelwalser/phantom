//! `phantom status` — show overlays, changesets, and system state.

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
        for agent in &active_agents {
            println!("  {agent}");
        }
    }
    println!();

    // Pending changesets
    let pending = projection.pending_changesets();
    if pending.is_empty() {
        println!("Pending changesets: (none)");
    } else {
        println!("Pending changesets:");
        println!("  {:<20} {:<14} {:>5}   {}", "ID", "AGENT", "FILES", "STATUS");
        for cs in &pending {
            println!(
                "  {:<20} {:<14} {:>5}   {:?}",
                cs.id,
                cs.agent_id,
                cs.files_touched.len(),
                cs.status,
            );
        }
    }
    println!();

    println!("Total events: {}", all_events.len());

    Ok(())
}
