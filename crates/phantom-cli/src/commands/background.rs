//! `phantom background` — real-time watch view of all background agents.
//!
//! Refreshes on a configurable interval (default 1s), showing each agent's
//! run state, elapsed time, and task description. Uses the alternate screen
//! buffer so previous terminal content is restored on exit.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use phantom_core::event::EventKind;
use phantom_core::id::AgentId;
use phantom_core::traits::EventStore;
use phantom_events::Projection;

use crate::context::PhantomContext;

use super::status::{self, AgentRunState};

#[derive(clap::Args)]
pub struct BackgroundArgs {
    /// Refresh interval in seconds
    #[arg(short = 'n', long = "interval", default_value = "1")]
    pub interval: f64,
}

pub async fn run(args: BackgroundArgs) -> anyhow::Result<()> {
    let interval = Duration::from_secs_f64(args.interval.max(0.1));

    let mut stdout = io::stdout();

    // Hide cursor while rendering.
    write!(stdout, "\x1b[?25l")?;
    stdout.flush()?;

    let result = run_loop(&mut stdout, interval).await;

    // Show cursor on exit.
    write!(stdout, "\x1b[?25h")?;
    stdout.flush()?;

    result
}

async fn run_loop(stdout: &mut io::Stdout, interval: Duration) -> anyhow::Result<()> {
    let mut prev_lines = 0usize;

    loop {
        // Move cursor up to overwrite the previous frame.
        if prev_lines > 0 {
            write!(stdout, "\x1b[{prev_lines}A\r")?;
        }

        let mut buf = Vec::new();
        if let Err(e) = render_frame(&mut buf).await {
            writeln!(buf, "\x1b[31mError: {e:#}\x1b[0m")?;
        }

        let output = String::from_utf8_lossy(&buf);
        let line_count = output.lines().count();

        // Clear each line as we write to handle shrinking output.
        for line in output.lines() {
            writeln!(stdout, "\x1b[2K{line}")?;
        }

        // If previous frame had more lines, clear the leftover lines.
        if prev_lines > line_count {
            for _ in 0..(prev_lines - line_count) {
                writeln!(stdout, "\x1b[2K")?;
            }
            // Move cursor back up to end of current frame.
            let extra = prev_lines - line_count;
            write!(stdout, "\x1b[{extra}A")?;
        }

        prev_lines = line_count;
        stdout.flush()?;

        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => {
                writeln!(stdout)?;
                return Ok(());
            }
        }
    }
}

async fn render_frame(out: &mut impl Write) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let events_store = ctx.open_events().await?;
    let git = ctx.open_git()?;
    let all_events = events_store.query_all().await?;
    let projection = Projection::from_events(&all_events);

    let head = git.head_oid()?;
    let head_short = head.to_hex();
    let head_short = &head_short[..12.min(head_short.len())];

    let now = chrono::Local::now().format("%H:%M:%S");

    // Header
    writeln!(out, "\x1b[1mphantom background\x1b[0m — watching agents  \x1b[2m{now}  (Ctrl+C to exit)\x1b[0m")?;
    writeln!(out)?;
    writeln!(out, "Trunk HEAD: \x1b[36m{head_short}\x1b[0m")?;
    writeln!(out)?;

    // Gather all agents (active changesets).
    let active_agents = projection.active_agents();

    // Also find agents that have completed (materialized/failed) — they still
    // have overlay dirs with status files.
    let mut all_agents: Vec<AgentId> = active_agents.clone();
    collect_completed_agents(&ctx.phantom_dir, &mut all_agents);
    all_agents.sort_by(|a, b| a.0.cmp(&b.0));
    all_agents.dedup();

    if all_agents.is_empty() {
        writeln!(out, "\x1b[2m  No agents found. Use `phantom task --background` to start one.\x1b[0m")?;
        return Ok(());
    }

    // Table header
    let task_header = "TASK";
    writeln!(
        out,
        "  {:<16} {:<14} {:<10} {task_header}",
        "AGENT", "STATUS", "ELAPSED"
    )?;
    writeln!(out, "  {}", "─".repeat(70))?;

    let mut running = 0usize;
    let mut finished = 0usize;
    let mut failed = 0usize;
    let mut idle = 0usize;

    for agent in &all_agents {
        let run_state = status::read_agent_run_state(&ctx.phantom_dir, &agent.0);

        match &run_state {
            AgentRunState::Running { .. } => running += 1,
            AgentRunState::Finished => finished += 1,
            AgentRunState::Failed { .. } => failed += 1,
            AgentRunState::Idle => idle += 1,
        }

        let (indicator, state_label, elapsed_str) = format_state_columns(&run_state);

        // Find task description.
        let task = all_events
            .iter()
            .rev()
            .find(|e| e.agent_id == *agent && matches!(e.kind, EventKind::TaskCreated { .. }))
            .and_then(|e| match &e.kind {
                EventKind::TaskCreated { task, .. } if !task.is_empty() => {
                    Some(task.as_str())
                }
                _ => None,
            })
            .unwrap_or("");

        let truncated_task = if task.len() > 45 {
            format!("{}...", &task[..42])
        } else {
            task.to_string()
        };

        writeln!(
            out,
            "  {:<16} {}{:<14}\x1b[0m {:<10} {}",
            agent.0, indicator, state_label, elapsed_str, truncated_task
        )?;
    }

    writeln!(out)?;
    write!(out, "  ")?;
    if running > 0 {
        write!(out, "\x1b[33m● {running} running\x1b[0m  ")?;
    }
    if finished > 0 {
        write!(out, "\x1b[32m✓ {finished} finished\x1b[0m  ")?;
    }
    if failed > 0 {
        write!(out, "\x1b[31m✗ {failed} failed\x1b[0m  ")?;
    }
    if idle > 0 {
        write!(out, "\x1b[2m○ {idle} idle\x1b[0m  ")?;
    }
    writeln!(out)?;

    Ok(())
}

/// Format run state into (color+indicator, label, elapsed) columns.
fn format_state_columns(state: &AgentRunState) -> (&'static str, String, String) {
    match state {
        AgentRunState::Running { pid: _, elapsed } => (
            "\x1b[33m● ",
            "running".into(),
            status::format_duration(elapsed),
        ),
        AgentRunState::Finished => {
            ("\x1b[32m✓ ", "finished".into(), String::new())
        }
        AgentRunState::Failed { status: s } => {
            let label = if let Some(s) = s {
                s.exit_code
                    .map(|c| format!("exit {c}"))
                    .unwrap_or_else(|| "signal".into())
            } else {
                "crashed".into()
            };
            ("\x1b[31m✗ ", label, String::new())
        }
        AgentRunState::Idle => ("\x1b[2m○ ", "idle".into(), String::new()),
    }
}

/// Scan `.phantom/overlays/` for agent directories that have a status file
/// (completed agents whose changesets may no longer be InProgress).
fn collect_completed_agents(phantom_dir: &Path, agents: &mut Vec<AgentId>) {
    let overlays_dir = phantom_dir.join("overlays");
    if let Ok(entries) = std::fs::read_dir(&overlays_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && let Some(name) = entry.file_name().to_str()
            {
                let id = AgentId(name.to_string());
                if !agents.contains(&id) {
                    agents.push(id);
                }
            }
        }
    }
}
