//! `phantom status` — show overlays, changesets, and system state.
//!
//! With no arguments, shows a summary of all active agents and pending
//! changesets. With an agent name, shows detailed info for that agent
//! including log output and file changes.

mod detail;
mod run_state;
mod summary;

use phantom_overlay::OverlayManager;

use crate::context::PhantomContext;

pub use run_state::{AgentRunState, format_duration, read_agent_run_state};
pub use summary::extract_plan_prefix;

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Show detailed status for a specific agent
    pub agent: Option<String>,

    /// Show all modified files instead of truncating the list
    #[arg(short, long)]
    pub all: bool,
}

pub async fn run(args: StatusArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let agent_ids = OverlayManager::scan_agent_ids(&ctx.phantom_dir)?;

    if let Some(agent_name) = &args.agent {
        detail::run_detailed(&ctx, &events, &agent_ids, agent_name, args.all).await
    } else {
        let git = ctx.open_git()?;
        summary::run_summary(&ctx, &git, &events, &agent_ids).await
    }
}
