//! `phantom dispatch` — assign a task to a new agent overlay.

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct DispatchArgs {
    /// Agent identifier (e.g. "agent-a")
    #[arg(long)]
    pub agent: String,
    /// Task description
    #[arg(long)]
    pub task: String,
}

pub async fn run(args: DispatchArgs) -> anyhow::Result<()> {
    let mut ctx = PhantomContext::load()?;

    let agent_id = AgentId(args.agent.clone());
    let head = ctx.git.head_oid().context("failed to read HEAD")?;

    let changeset_id = generate_changeset_id(&ctx)?;

    let handle = ctx
        .overlays
        .create_overlay(agent_id.clone(), &ctx.repo_root)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mount_point = handle.mount_point.clone();

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::OverlayCreated { base_commit: head },
    };
    ctx.events
        .append(event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Agent '{}' dispatched.", args.agent);
    println!("  Changeset: {changeset_id}");
    println!("  Task:      {}", args.task);
    println!("  Overlay:   {}", mount_point.display());
    println!(
        "  Base:      {}",
        head.to_hex().chars().take(12).collect::<String>()
    );
    Ok(())
}

/// Generate a unique changeset ID.
///
/// Uses a monotonic counter from the event store combined with a timestamp
/// suffix to avoid collisions from concurrent dispatch calls.
fn generate_changeset_id(ctx: &PhantomContext) -> anyhow::Result<ChangesetId> {
    let events = ctx.events.query_all().map_err(|e| anyhow::anyhow!("{e}"))?;

    let overlay_count = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::OverlayCreated { .. }))
        .count();

    // Append timestamp micros to avoid race condition when two dispatches
    // read the same count concurrently.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        % 1_000_000;

    Ok(ChangesetId(format!("cs-{:04}-{ts:06}", overlay_count + 1)))
}
