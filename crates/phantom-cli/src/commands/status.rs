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
        println!("  {:<20} {:<15}", "AGENT", "STATUS");
        for agent in &active_agents {
            let mount = ctx
                .overlays
                .upper_dir(agent)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "(not mounted)".into());
            println!("  {:<20} {}", agent, mount);
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
