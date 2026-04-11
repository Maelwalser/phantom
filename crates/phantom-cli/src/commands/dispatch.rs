//! `phantom dispatch` — create an agent overlay and launch a Claude Code session.
//!
//! By default, dispatching opens an interactive Claude Code CLI inside the
//! overlay. Use `--background` to create the overlay without launching a
//! session (for scripted / headless agents).

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct DispatchArgs {
    /// Agent identifier (e.g. "agent-a")
    pub agent: String,
    /// Task description for the agent (only available with --background)
    #[arg(long, requires = "background")]
    pub task: Option<String>,
    /// Create the overlay without launching a CLI session (for scripted agents)
    #[arg(long, short = 'b', requires = "task")]
    pub background: bool,
    /// Automatically submit the changeset when the interactive session exits
    #[arg(long, conflicts_with = "background")]
    pub auto_submit: bool,
    /// Automatically materialize after submitting (implies --auto-submit)
    #[arg(long, conflicts_with = "background")]
    pub auto_materialize: bool,
    /// Custom command to run instead of `claude` (e.g. for testing)
    #[arg(long, conflicts_with = "background")]
    pub command: Option<String>,
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
        kind: EventKind::OverlayCreated {
            base_commit: head,
            task: args.task.clone().unwrap_or_default(),
        },
    };
    ctx.events
        .append(event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let base_short = head.to_hex().chars().take(12).collect::<String>();

    if args.background {
        let task = args.task.as_deref().unwrap_or("");

        // Write context file so the background agent knows its task
        let upper_dir = ctx
            .overlays
            .upper_dir(&agent_id)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        super::interactive::write_context_file(
            upper_dir,
            &agent_id,
            &changeset_id,
            &head,
            Some(task),
        )?;

        println!("Agent '{}' dispatched (background).", args.agent);
        println!("  Changeset: {changeset_id}");
        println!("  Task:      {task}");
        println!("  Overlay:   {}", mount_point.display());
        println!("  Base:      {base_short}");
    } else {
        println!("Agent '{}' dispatched.", args.agent);
        println!("  Changeset: {changeset_id}");
        println!("  Overlay:   {}", mount_point.display());
        println!("  Base:      {base_short}");
        println!();
        super::interactive::run_interactive_session(
            &mut ctx,
            &agent_id,
            &changeset_id,
            &head,
            &args,
        )?;
    }

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
