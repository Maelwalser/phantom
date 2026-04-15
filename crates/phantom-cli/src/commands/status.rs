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
use phantom_events::{Projection, SnapshotManager, SqliteEventStore};
use phantom_git::GitOps;
use phantom_overlay::OverlayManager;

use super::agent_monitor;
use super::ui;
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
    Running { pid: u32, elapsed: Duration },
    /// Agent monitor is waiting for upstream dependencies to materialize.
    WaitingForDependencies { upstream: Vec<String> },
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
        run_summary(&ctx.phantom_dir, &git, &events, &overlays).await
    }
}

/// Summary view: show all overlays, pending changesets, and conflicts.
async fn run_summary(
    phantom_dir: &Path,
    git: &GitOps,
    events: &SqliteEventStore,
    overlays: &OverlayManager,
) -> anyhow::Result<()> {
    let head = git.head_oid()?;

    let projection = SnapshotManager::new(events).build_projection().await?;
    let all_events = events.query_all().await?;

    // Header
    let head_short = head.to_hex();
    let head_short = &head_short[..12.min(head_short.len())];
    println!(
        "{} {}",
        ui::style_dim("Trunk HEAD:"),
        ui::style_cyan(head_short)
    );
    println!();

    // All overlays that exist on disk.
    let all_handles = overlays.list_overlays();
    let mut overlay_agents: Vec<&AgentId> = all_handles.iter().map(|h| &h.agent_id).collect();
    overlay_agents.sort_by(|a, b| a.0.cmp(&b.0));

    // Detect plans: find PlanCreated events to map plan IDs to their request text.
    let mut plan_agents: std::collections::HashMap<String, Vec<&AgentId>> =
        std::collections::HashMap::new();
    let mut standalone_agents: Vec<&AgentId> = Vec::new();
    let mut plan_requests: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // Collect plan metadata from events.
    for event in &all_events {
        if let EventKind::PlanCreated {
            plan_id, request, ..
        } = &event.kind
        {
            plan_requests.insert(plan_id.0.clone(), request.clone());
        }
    }

    // Classify agents into plan groups vs standalone.
    for agent in &overlay_agents {
        let plan_prefix = extract_plan_prefix(&agent.0);
        if let Some(prefix) = plan_prefix {
            plan_agents.entry(prefix).or_default().push(agent);
        } else {
            standalone_agents.push(agent);
        }
    }

    if overlay_agents.is_empty() {
        println!("  {} {}", ui::style_bold("Overlays:"), ui::style_dim("(none)"));
    } else {
        ui::section_header("Overlays");

        let width = ui::term_width();

        // Print plan groups first.
        for (plan_prefix, agents) in &plan_agents {
            // Prefix: "  Plan: " + plan_prefix + " — "
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
                let run_state = read_agent_run_state(phantom_dir, &agent.0);
                let indicator = ui::run_state_indicator(&run_state);
                let state_text = ui::run_state_text(&run_state);
                let status = latest_changeset_status(&projection, agent);
                let domain_name = agent
                    .0
                    .strip_prefix(&format!("{plan_prefix}-"))
                    .unwrap_or(&agent.0);
                println!(
                    "    {indicator} {domain_name:<20} {state_text:<12} {status}"
                );
            }
            println!();
        }

        // Print standalone agents.
        for agent in &standalone_agents {
            let run_state = read_agent_run_state(phantom_dir, &agent.0);
            let indicator = ui::run_state_indicator(&run_state);
            let state_text = ui::run_state_text(&run_state);
            let elapsed_raw = match &run_state {
                AgentRunState::Running { elapsed, .. } => Some(format_duration(elapsed)),
                _ => None,
            };
            let elapsed = elapsed_raw
                .as_ref()
                .map(|e| format!(" {}", ui::style_dim(e)))
                .unwrap_or_default();
            let status = latest_changeset_status(&projection, agent);

            let task = all_events
                .iter()
                .rev()
                .find(|e| e.agent_id == **agent && matches!(e.kind, EventKind::TaskCreated { .. }))
                .and_then(|e| match &e.kind {
                    EventKind::TaskCreated { task, .. } if !task.is_empty() => Some(task.as_str()),
                    _ => None,
                });

            if let Some(task) = task {
                // Prefix: "  " + indicator(2) + " " + agent(14) + " " + state(12) + " elapsed" + " " + status + "  "
                let elapsed_visible = elapsed_raw.as_ref().map_or(0, |e| e.len() + 1);
                let status_visible = console::measure_text_width(&format!("{status}"));
                let prefix_len = 2 + 2 + 1 + 14 + 1 + 12 + elapsed_visible + 1 + status_visible + 2;
                let truncated = ui::truncate_line(task, width.saturating_sub(prefix_len));
                println!(
                    "  {indicator} {agent:<14} {state_text:<12}{elapsed} {status}  {}",
                    ui::style_dim(&truncated)
                );
            } else {
                println!("  {indicator} {agent:<14} {state_text:<12}{elapsed} {status}");
            }
        }
    }
    println!();

    // Pending: overlays with modified files that haven't been submitted yet.
    let mut pending_overlays: Vec<(&AgentId, usize)> = Vec::new();
    for handle in &all_handles {
        // Only show overlays whose latest changeset is still InProgress.
        let changesets = projection.changesets_for_agent(&handle.agent_id);
        let is_pending = changesets
            .first()
            .is_some_and(|cs| cs.status == ChangesetStatus::InProgress);
        if !is_pending {
            continue;
        }
        if let Ok(layer) = overlays.get_layer(&handle.agent_id)
            && let Ok(files) = layer.modified_files()
            && !files.is_empty()
        {
            pending_overlays.push((&handle.agent_id, files.len()));
        }
    }
    if pending_overlays.is_empty() {
        println!(
            "  {} {}",
            ui::style_bold("Pending changesets:"),
            ui::style_dim("(none)")
        );
    } else {
        ui::section_header("Pending changesets");
        println!(
            "  {}",
            ui::style_dim(&format!("{:<14} {:>5}", "AGENT", "FILES"))
        );
        for (agent_id, file_count) in &pending_overlays {
            println!(
                "  {agent_id:<14} {file_count:>5}",
            );
        }
    }
    println!();

    // Conflicted changesets
    let conflicted = projection.conflicted_changesets();
    if !conflicted.is_empty() {
        ui::section_header("Conflicted changesets");
        println!(
            "  {}",
            ui::style_dim(&format!(
                "{:<20} {:<14} {:>5}   STATUS",
                "ID", "AGENT", "FILES"
            ))
        );
        for cs in &conflicted {
            println!(
                "  {:<20} {:<14} {:>5}   {}",
                cs.id,
                cs.agent_id,
                cs.files_touched.len(),
                ui::status_label(cs.status),
            );
        }
        println!();
    }

    println!(
        "{}",
        ui::style_dim(&format!("Total events: {}", all_events.len()))
    );

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

    let projection = SnapshotManager::new(events).build_projection().await?;
    let all_events = events.query_all().await?;

    // Find the changeset for this agent.
    let agent_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.agent_id == agent_id)
        .collect();

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
            console::Style::new().dim().apply_to(format!("{:<12}", "Status"))
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
        console::Style::new().dim().apply_to(format!("{:<12}", "Run state")),
        format_run_state_long(&run_state)
    );
    println!();

    // Files modified in overlay
    match overlays.get_layer(&agent_id) {
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
                println!("  {} {}", ui::style_bold("Modified files:"), ui::style_error(&format!("(error: {e})")));
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

    // Log tail
    let log_file = agent_monitor::log_path(phantom_dir, agent_name);
    if log_file.exists()
        && let Some(tail) = read_log_tail(&log_file, 20)
    {
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
        && let Ok(status) = serde_json::from_str::<agent_monitor::AgentStatus>(&content)
    {
        return if status.exit_code == Some(0) && status.error.is_none() {
            AgentRunState::Finished
        } else {
            AgentRunState::Failed {
                status: Some(status),
            }
        };
    }

    // Check for dependency wait marker (monitor running, claude not yet spawned).
    let waiting_file = phantom_dir
        .join("overlays")
        .join(agent)
        .join("waiting.json");
    if let Ok(content) = std::fs::read_to_string(&waiting_file) {
        // Verify the monitor is still alive (with PID reuse protection).
        let monitor_pid_file = agent_monitor::monitor_pid_path(phantom_dir, agent);
        let monitor_alive = crate::pid_guard::read_pid_file(&monitor_pid_file)
            .is_some_and(|r| crate::pid_guard::is_process_alive(&r));

        if monitor_alive {
            let upstream: Vec<String> = serde_json::from_str(&content).unwrap_or_default();
            return AgentRunState::WaitingForDependencies { upstream };
        }
        // Monitor died while waiting — clean up marker and fall through to Failed.
        let _ = std::fs::remove_file(&waiting_file);
    }

    // Check for running process (with PID reuse protection).
    let pid_file = agent_monitor::pid_path(phantom_dir, agent);
    if let Some(record) = crate::pid_guard::read_pid_file(&pid_file) {
        if crate::pid_guard::is_process_alive(&record) {
            // Estimate elapsed time from PID file modification time.
            let elapsed = std::fs::metadata(&pid_file)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            return AgentRunState::Running {
                pid: record.pid as u32,
                elapsed,
            };
        }

        // Process is dead but no status file — crashed.
        return AgentRunState::Failed { status: None };
    }

    AgentRunState::Idle
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
                    .exit_code.map_or_else(|| "killed by signal".into(), |c| format!("exit code {c}"));
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

/// Format a duration as "Xh Ym Zs" or "Xm Zs" or "Zs".
pub fn format_duration(d: &Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else if secs == 0 {
        "just started".to_string()
    } else {
        format!("{secs}s")
    }
}

/// Extract the plan prefix from an agent ID, if it matches the plan naming pattern.
///
/// Plan agent IDs follow: `plan-YYYYMMDD-HHMMSS-domain-name`.
/// Returns `Some("plan-YYYYMMDD-HHMMSS")` if matched.
fn extract_plan_prefix(agent_id: &str) -> Option<String> {
    if !agent_id.starts_with("plan-") {
        return None;
    }
    // Expected format: plan-YYYYMMDD-HHMMSS-rest
    // The prefix is the first 22 characters: "plan-" (5) + "YYYYMMDD" (8) + "-" (1) + "HHMMSS" (6) + "-" (1) = 21
    // But we need to be flexible. Split by '-' and take plan + date + time.
    let parts: Vec<&str> = agent_id.splitn(4, '-').collect();
    if parts.len() >= 4 && parts[1].len() == 8 && parts[2].len() == 6 {
        Some(format!("{}-{}-{}", parts[0], parts[1], parts[2]))
    } else {
        None
    }
}

/// Get the styled status label for the latest changeset of an agent.
fn latest_changeset_status(projection: &Projection, agent_id: &AgentId) -> String {
    let changesets = projection.changesets_for_agent(agent_id);
    match changesets.first() {
        Some(cs) => format!("{}", ui::status_label(cs.status)),
        None => format!("{}", ui::style_dim("no changeset")),
    }
}

/// Read the last N lines of a log file.
fn read_log_tail(path: &PathBuf, n: usize) -> Option<String> {
    use std::collections::VecDeque;

    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut ring = VecDeque::with_capacity(n);
    for line in reader.lines().map_while(Result::ok) {
        if ring.len() == n {
            ring.pop_front();
        }
        ring.push_back(line);
    }
    if ring.is_empty() {
        None
    } else {
        let tail: Vec<&str> = ring.iter().map(String::as_str).collect();
        Some(tail.join("\n"))
    }
}
