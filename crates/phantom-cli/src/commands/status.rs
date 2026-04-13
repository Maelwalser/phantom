//! `phantom status` — show overlays, changesets, and system state.
//!
//! With no arguments, shows a summary of all active agents and pending
//! changesets. With an agent name, shows detailed info for that agent
//! including log output and file changes.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use phantom_core::changeset::ChangesetStatus;
use phantom_core::event::EventKind;
use phantom_core::id::AgentId;
use phantom_core::traits::EventStore;
use phantom_events::{Projection, SqliteEventStore};
use phantom_orchestrator::git::GitOps;
use phantom_overlay::OverlayManager;

use super::agent_monitor;
use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Show detailed status for a specific agent
    pub agent: Option<String>,
}

/// Run state of a background agent process.
#[derive(Debug)]
pub enum AgentRunState {
    /// Agent process is currently running.
    /// Agent process is currently running.
    Running {
        pid: u32,
        elapsed: Duration,
    },
    /// Agent process finished successfully.
    Finished,
    /// Agent process failed or crashed.
    Failed {
        status: Option<agent_monitor::AgentStatus>,
    },
    /// No background process — interactive or not yet launched.
    Idle,
}

pub async fn run(args: StatusArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let git = ctx.open_git()?;
    let events = ctx.open_events().await?;
    let overlays = ctx.open_overlays_restored()?;

    if let Some(agent_name) = &args.agent {
        run_detailed(&ctx.phantom_dir, &events, &overlays, agent_name).await
    } else {
        run_summary(&ctx.phantom_dir, &git, &events).await
    }
}

/// Summary view: show all active agents and pending changesets.
async fn run_summary(
    phantom_dir: &Path,
    git: &GitOps,
    events: &SqliteEventStore,
) -> anyhow::Result<()> {
    let head = git.head_oid()?;

    let all_events = events.query_all().await?;
    let projection = Projection::from_events(&all_events);

    // Header
    let head_short = head.to_hex();
    let head_short = &head_short[..12.min(head_short.len())];
    println!("Trunk HEAD: {head_short}");
    println!();

    // Active overlays with run state
    let active_agents = projection.active_agents();
    if active_agents.is_empty() {
        println!("Active overlays: (none)");
    } else {
        println!("Active overlays:");
        for agent in &active_agents {
            let run_state = read_agent_run_state(phantom_dir, &agent.0);
            let state_str = format_run_state_short(&run_state);

            // Find the task description from the most recent OverlayCreated event.
            let task = all_events
                .iter()
                .rev()
                .find(|e| e.agent_id == *agent && matches!(e.kind, EventKind::OverlayCreated { .. }))
                .and_then(|e| match &e.kind {
                    EventKind::OverlayCreated { task, .. } if !task.is_empty() => Some(task.as_str()),
                    _ => None,
                });

            if let Some(task) = task {
                let truncated = if task.len() > 50 {
                    format!("{}...", &task[..47])
                } else {
                    task.to_string()
                };
                println!("  {agent:<14} {state_str:<28} {truncated}");
            } else {
                println!("  {agent:<14} {state_str}");
            }
        }
    }
    println!();

    // Pending changesets
    let pending = projection.pending_changesets();
    if pending.is_empty() {
        println!("Pending changesets: (none)");
    } else {
        println!("Pending changesets:");
        println!(
            "  {:<20} {:<14} {:>5}   STATUS",
            "ID", "AGENT", "FILES"
        );
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

/// Detailed view for a specific agent.
async fn run_detailed(
    phantom_dir: &Path,
    events: &SqliteEventStore,
    overlays: &OverlayManager,
    agent_name: &str,
) -> anyhow::Result<()> {
    let agent_id = AgentId(agent_name.to_string());

    let all_events = events.query_all().await?;
    let projection = Projection::from_events(&all_events);

    // Find the changeset for this agent.
    let agent_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.agent_id == agent_id)
        .collect();

    if agent_events.is_empty() {
        anyhow::bail!("no events found for agent '{agent_name}'");
    }

    // Find changeset ID and task from most recent OverlayCreated.
    let (changeset_id, task) = agent_events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::OverlayCreated { task, .. } => {
                Some((e.changeset_id.clone(), task.clone()))
            }
            _ => None,
        })
        .context("no overlay found for this agent")?;

    let changeset = projection.changeset(&changeset_id);

    println!("Agent: {agent_name}");
    println!("Changeset: {changeset_id}");
    if !task.is_empty() {
        println!("Task: {task}");
    }

    if let Some(cs) = changeset {
        println!("Status: {:?}", cs.status);
        let base_hex = cs.base_commit.to_hex();
        println!("Base: {}", &base_hex[..12.min(base_hex.len())]);
    }
    println!();

    // Background agent run state
    let run_state = read_agent_run_state(phantom_dir, agent_name);
    println!("Run state: {}", format_run_state_long(&run_state));
    println!();

    // Files modified in overlay
    match overlays.get_layer(&agent_id) {
        Ok(layer) => match layer.modified_files() {
            Ok(files) if !files.is_empty() => {
                println!("Modified files ({}):", files.len());
                for f in &files {
                    println!("  {}", f.display());
                }
                println!();
            }
            Ok(_) => {
                println!("Modified files: (none)");
                println!();
            }
            Err(e) => {
                println!("Modified files: (error: {e})");
                println!();
            }
        },
        Err(_) => {
            if let Some(cs) = changeset {
                if cs.status == ChangesetStatus::Materialized {
                    println!("Overlay: cleared (materialized)");
                } else {
                    println!("Overlay: not found");
                }
                println!();
            }
        }
    }

    // Log tail
    let log_file = agent_monitor::log_path(phantom_dir, agent_name);
    if log_file.exists()
        && let Some(tail) = read_log_tail(&log_file, 20) {
            println!("Log (last 20 lines):");
            println!("{tail}");
        }

    Ok(())
}

/// Read the run state of a background agent from disk.
pub fn read_agent_run_state(phantom_dir: &Path, agent: &str) -> AgentRunState {
    // Check for completion marker first.
    let status_file = agent_monitor::status_path(phantom_dir, agent);
    if let Ok(content) = std::fs::read_to_string(&status_file)
        && let Ok(status) = serde_json::from_str::<agent_monitor::AgentStatus>(&content) {
            return if status.exit_code == Some(0) && status.error.is_none() {
                AgentRunState::Finished
            } else {
                AgentRunState::Failed {
                    status: Some(status),
                }
            };
        }

    // Check for running process.
    let pid_file = agent_monitor::pid_path(phantom_dir, agent);
    if let Ok(content) = std::fs::read_to_string(&pid_file)
        && let Ok(pid) = content.trim().parse::<u32>() {
            // Check if process is still alive.
            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
            if alive {
                // Estimate elapsed time from PID file modification time.
                let elapsed = std::fs::metadata(&pid_file)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .unwrap_or_default();
                return AgentRunState::Running { pid, elapsed };
            }

            // Process is dead but no status file — crashed.
            return AgentRunState::Failed { status: None };
        }

    AgentRunState::Idle
}

/// Format run state for the summary table.
pub fn format_run_state_short(state: &AgentRunState) -> String {
    match state {
        AgentRunState::Running { pid, elapsed } => {
            format!("running {}  pid {pid}", format_duration(elapsed))
        }
        AgentRunState::Finished => "finished".into(),
        AgentRunState::Failed { status } => {
            if let Some(s) = status {
                let code = s
                    .exit_code
                    .map(|c| format!("exit {c}"))
                    .unwrap_or_else(|| "signal".into());
                format!("failed  {code}")
            } else {
                "failed  (crashed)".into()
            }
        }
        AgentRunState::Idle => "idle  (interactive)".into(),
    }
}

/// Format run state for the detailed view.
fn format_run_state_long(state: &AgentRunState) -> String {
    match state {
        AgentRunState::Running { pid, elapsed } => {
            format!(
                "Running (pid {pid}, elapsed {})",
                format_duration(elapsed)
            )
        }
        AgentRunState::Finished => "Finished".into(),
        AgentRunState::Failed { status } => {
            if let Some(s) = status {
                let code = s
                    .exit_code
                    .map(|c| format!("exit code {c}"))
                    .unwrap_or_else(|| "killed by signal".into());
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
        AgentRunState::Idle => "Idle (no background process)".into(),
    }
}

/// Format a duration as "Xh Ym Zs" or "Xm Zs" or "Zs".
pub fn format_duration(d: &Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Read the last N lines of a log file.
fn read_log_tail(path: &PathBuf, n: usize) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
    let start = lines.len().saturating_sub(n);
    let tail = &lines[start..];
    if tail.is_empty() {
        None
    } else {
        Some(tail.join("\n"))
    }
}
