//! `phantom resume` — select and resume an interactive agent session.
//!
//! Presents a `dialoguer::Select` menu of non-background agent overlays
//! and delegates to `task::run()` to resume the selected agent.

use dialoguer::Select;
use phantom_core::id::AgentId;
use phantom_events::SnapshotManager;

use super::status::{self, AgentRunState};
use crate::context::PhantomContext;
use crate::ui;

#[derive(clap::Args)]
pub struct ResumeArgs {}

pub async fn run(_args: ResumeArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let agent_ids = phantom_overlay::OverlayManager::scan_agent_ids(&ctx.phantom_dir)?;

    if agent_ids.is_empty() {
        ui::empty_state(
            "No active tasks.",
            Some("Use `phantom <agent>` to create one."),
        );
        return Ok(());
    }

    // Filter to non-background agents (Idle run state = no agent.pid / agent.status).
    let mut interactive_agents: Vec<&AgentId> = agent_ids
        .iter()
        .filter(|a| {
            matches!(
                status::read_agent_run_state(&ctx.phantom_dir, &a.0),
                AgentRunState::Idle
            )
        })
        .collect();
    interactive_agents.sort_by(|a, b| a.0.cmp(&b.0));

    if interactive_agents.is_empty() {
        ui::empty_state(
            "No interactive agents to resume.",
            Some("All agents are running in the background."),
        );
        return Ok(());
    }

    let projection = SnapshotManager::new(&events).build_projection().await?;

    let width = ui::term_width();
    let display_items: Vec<String> = interactive_agents
        .iter()
        .map(|agent| {
            let changesets = projection.changesets_for_agent(agent);
            match changesets.first() {
                Some(cs) => {
                    let status_str = format!("{}", ui::status_label(cs.status));
                    let status_width = console::measure_text_width(&status_str);
                    let age = ui::relative_time(cs.created_at);

                    let prefix_len = 16 + 2 + status_width + 2 + 2 + age.len() + 1;
                    let task_max = width.saturating_sub(prefix_len).max(10);
                    let task = if cs.task.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", ui::truncate_line(&cs.task, task_max))
                    };

                    format!("{:<16} {status_str}  ({}){task}", agent.0, age)
                }
                None => format!("{:<16} {}", agent.0, ui::style_dim("no changeset")),
            }
        })
        .collect();

    let selection = Select::new()
        .with_prompt("Select an agent to resume")
        .items(&display_items)
        .default(0)
        .interact_opt()?;

    let Some(idx) = selection else {
        println!("  {}", console::style("Cancelled.").dim());
        return Ok(());
    };

    let selected = interactive_agents[idx].0.clone();

    // Delegate to the task command which handles FUSE remounting, session
    // loading, and PTY spawning for existing overlays.
    super::task::run(super::task::TaskArgs {
        agent: selected,
        task: None,
        background: false,
        auto_submit: false,
        command: None,
        no_fuse: false,
        category: None,
    })
    .await
}
