//! Detailed per-agent view: task, base commit, file modifications, log output,
//! and background-agent run state.

use anyhow::Context;
use phantom_core::changeset::ChangesetStatus;
use phantom_core::event::EventKind;
use phantom_core::id::AgentId;
use phantom_core::traits::EventStore;
use phantom_events::{SnapshotManager, SqliteEventStore};

use super::super::agent_monitor;
use super::run_state::{AgentRunState, format_duration, read_agent_run_state};
use crate::context::PhantomContext;
use crate::ui;

/// Detailed view for a specific agent.
pub(super) async fn run_detailed(
    ctx: &PhantomContext,
    events: &SqliteEventStore,
    _agent_ids: &[AgentId],
    agent_name: &str,
) -> anyhow::Result<()> {
    let phantom_dir = &ctx.phantom_dir;
    let agent_id = AgentId(agent_name.to_string());

    let projection = SnapshotManager::new(events).build_projection().await?;
    let agent_events = events.query_by_agent(&agent_id).await?;

    if agent_events.is_empty() {
        anyhow::bail!("no events found for agent '{agent_name}'");
    }

    // Find changeset ID and task from most recent TaskCreated.
    let (changeset_id, task) = agent_events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::TaskCreated { task, .. } => Some((e.changeset_id.clone(), task.clone())),
            _ => None,
        })
        .context("no overlay found for this agent")?;

    let changeset = projection.changeset(&changeset_id);

    ui::key_value("Agent", agent_name);
    ui::key_value("Changeset", changeset_id.to_string());
    if !task.is_empty() {
        ui::key_value("Task", task);
    }

    if let Some(cs) = changeset {
        let status_styled = ui::status_label(cs.status);
        println!(
            "  {}  {status_styled}",
            console::Style::new()
                .dim()
                .apply_to(format!("{:<12}", "Status"))
        );
        let base_hex = cs.base_commit.to_hex();
        ui::key_value("Base", ui::style_cyan(&base_hex[..12.min(base_hex.len())]));
    }
    println!();

    // Background agent run state
    let run_state = read_agent_run_state(phantom_dir, agent_name);
    let indicator = ui::run_state_indicator(&run_state);
    println!(
        "  {}  {indicator} {}",
        console::Style::new()
            .dim()
            .apply_to(format!("{:<12}", "Run state")),
        format_run_state_long(&run_state)
    );
    println!();

    // Files modified in overlay — lazily create a single overlay for this agent.
    let mut mgr = ctx.open_overlays();
    let _ = mgr.create_overlay(agent_id.clone(), &ctx.repo_root);
    match mgr.get_layer(&agent_id) {
        Ok(layer) => match layer.modified_files() {
            Ok(files) if !files.is_empty() => {
                println!(
                    "  {} {}",
                    ui::style_bold("Modified files"),
                    ui::style_dim(&format!("({})", files.len()))
                );
                for f in &files {
                    println!("    {}", ui::style_dim(&f.display().to_string()));
                }
                println!();
            }
            Ok(_) => {
                println!(
                    "  {} {}",
                    ui::style_bold("Modified files:"),
                    ui::style_dim("(none)")
                );
                println!();
            }
            Err(e) => {
                println!(
                    "  {} {}",
                    ui::style_bold("Modified files:"),
                    ui::style_error(&format!("(error: {e})"))
                );
                println!();
            }
        },
        Err(_) => {
            if let Some(cs) = changeset {
                if cs.status == ChangesetStatus::Submitted {
                    ui::key_value("Overlay", ui::style_dim("cleared (materialized)"));
                } else {
                    ui::key_value("Overlay", ui::style_warning("not found"));
                }
                println!();
            }
        }
    }

    // Full log output
    let log_file = agent_monitor::log_path(phantom_dir, agent_name);
    if log_file.exists()
        && let Ok(content) = std::fs::read_to_string(&log_file)
        && !content.is_empty()
    {
        println!("{}:", console::style("Log").bold());
        print!("{content}");
    }

    Ok(())
}

/// Format run state for the detailed view.
fn format_run_state_long(state: &AgentRunState) -> String {
    match state {
        AgentRunState::Running { pid, elapsed } => {
            format!("Running (pid {pid}, elapsed {})", format_duration(elapsed))
        }
        AgentRunState::Finished => "Finished".into(),
        AgentRunState::Failed { status } => {
            if let Some(s) = status {
                let code = s
                    .exit_code
                    .map_or_else(|| "killed by signal".into(), |c| format!("exit code {c}"));
                let err = s
                    .error
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default();
                format!("Failed ({code}{err})")
            } else {
                "Failed (process crashed — no status written)".into()
            }
        }
        AgentRunState::WaitingForDependencies { upstream } => {
            format!("Waiting for: {}", upstream.join(", "))
        }
        AgentRunState::Idle => "Idle (no background process)".into(),
    }
}
