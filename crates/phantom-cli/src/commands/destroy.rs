//! `phantom destroy` — tear down an agent's overlay.

use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, EventId};
use phantom_core::traits::EventStore;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct DestroyArgs {
    /// Agent identifier whose overlay to destroy
    #[arg(long)]
    pub agent: String,
}

pub async fn run(args: DestroyArgs) -> anyhow::Result<()> {
    let mut ctx = PhantomContext::load()?;

    let agent_id = AgentId(args.agent.clone());

    // Find the changeset ID for this agent
    let events = ctx
        .events
        .query_by_agent(&agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let changeset_id = events
        .iter()
        .rev()
        .find_map(|e| {
            if matches!(e.kind, EventKind::OverlayCreated { .. }) {
                Some(e.changeset_id.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no overlay found for agent '{}' — was it dispatched?",
                args.agent
            )
        })?;

    ctx.overlays
        .destroy_overlay(&agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id,
        agent_id,
        kind: EventKind::OverlayDestroyed,
    };
    ctx.events
        .append(event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Agent '{}' overlay destroyed.", args.agent);
    Ok(())
}
