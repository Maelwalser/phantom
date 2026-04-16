//! `phantom tasks` — list all agent task overlays.

use std::path::Path;

use phantom_core::event::EventKind;
use phantom_core::id::AgentId;
use phantom_events::{Projection, SnapshotManager, SqliteEventStore};
use phantom_overlay::OverlayManager;

use super::status::{self, extract_plan_prefix};
use super::ui;
use crate::context::PhantomContext;



#[derive(clap::Args)]
pub struct TasksArgs {}

pub async fn run(_args: TasksArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let agent_ids = OverlayManager::scan_agent_ids(&ctx.phantom_dir)?;

    print_tasks(&ctx.phantom_dir, &events, &agent_ids).await
}

async fn print_tasks(
    phantom_dir: &Path,
    events: &SqliteEventStore,
    agent_ids: &[AgentId],
) -> anyhow::Result<()> {
    if agent_ids.is_empty() {
        println!("No active tasks.");
        return Ok(());
    }

    let projection = SnapshotManager::new(events).build_projection().await?;

    let mut overlay_agents: Vec<&AgentId> = agent_ids.iter().collect();
    overlay_agents.sort_by(|a, b| a.0.cmp(&b.0));

    // Collect plan metadata for grouping.
    let plan_query = phantom_events::query::EventQuery {
        kind_prefixes: vec!["PlanCreated".into()],
        ..Default::default()
    };
    let plan_events = events.query(&plan_query).await?;
    let mut plan_requests: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for event in &plan_events {
        if let EventKind::PlanCreated {
            plan_id, request, ..
        } = &event.kind
        {
            plan_requests.insert(plan_id.0.clone(), request.clone());
        }
    }

    // Classify agents into plan groups vs standalone.
    let mut plan_agents: std::collections::HashMap<String, Vec<&AgentId>> =
        std::collections::HashMap::new();
    let mut standalone_agents: Vec<&AgentId> = Vec::new();

    for agent in &overlay_agents {
        if let Some(prefix) = extract_plan_prefix(&agent.0) {
            plan_agents.entry(prefix).or_default().push(agent);
        } else {
            standalone_agents.push(agent);
        }
    }

    let width = ui::term_width();

    ui::section_header("Tasks");

    // Plan groups.
    for (plan_prefix, agents) in &plan_agents {
        let prefix_len = 2 + 6 + plan_prefix.len() + 3;
        let request = plan_requests
            .get(plan_prefix)
            .map(|r| ui::truncate_line(r, width.saturating_sub(prefix_len)))
            .unwrap_or_default();
        println!(
            "  {} {} {} {}",
            ui::style_dim("Plan:"),
            ui::style_cyan(plan_prefix),
            ui::style_dim("—"),
            request
        );

        for agent in agents {
            let run_state = status::read_agent_run_state(phantom_dir, &agent.0);
            let indicator = ui::run_state_indicator(&run_state);
            let state_text = ui::run_state_text(&run_state);
            let cs_status = latest_changeset_status(&projection, agent);
            let domain_name = agent
                .0
                .strip_prefix(&format!("{plan_prefix}-"))
                .unwrap_or(&agent.0);
            println!("    {indicator} {domain_name:<20} {state_text:<12} {cs_status}");
        }
        println!();
    }

    // Standalone agents.
    for agent in &standalone_agents {
        let run_state = status::read_agent_run_state(phantom_dir, &agent.0);
        let indicator = ui::run_state_indicator(&run_state);
        let state_text = ui::run_state_text(&run_state);
        let elapsed_str = match &run_state {
            status::AgentRunState::Running { elapsed, .. } => {
                Some(status::format_duration(elapsed))
            }
            _ => None,
        };
        let elapsed = elapsed_str
            .as_ref()
            .map(|e| format!(" {}", ui::style_dim(e)))
            .unwrap_or_default();
        let cs_status = latest_changeset_status(&projection, agent);

        let task = projection
            .changesets_for_agent(agent)
            .first()
            .map(|cs| cs.task.as_str())
            .filter(|t| !t.is_empty());

        if let Some(task) = task {
            let elapsed_visible = elapsed_str.as_ref().map_or(0, |e| e.len() + 1);
            let status_visible = console::measure_text_width(&cs_status);
            let prefix_len = 2 + 2 + 1 + 14 + 1 + 12 + elapsed_visible + 1 + status_visible + 2;
            let truncated = ui::truncate_line(task, width.saturating_sub(prefix_len));
            println!(
                "  {indicator} {agent:<14} {state_text:<12}{elapsed} {cs_status}  {}",
                ui::style_dim(&truncated)
            );
        } else {
            println!("  {indicator} {agent:<14} {state_text:<12}{elapsed} {cs_status}");
        }
    }

    let total = overlay_agents.len();
    println!();
    println!("{}", ui::style_dim(&format!("{total} task(s)")));

    Ok(())
}

fn latest_changeset_status(projection: &Projection, agent_id: &AgentId) -> String {
    let changesets = projection.changesets_for_agent(agent_id);
    match changesets.first() {
        Some(cs) => format!("{}", ui::status_label(cs.status)),
        None => format!("{}", ui::style_dim("no changeset")),
    }
}
