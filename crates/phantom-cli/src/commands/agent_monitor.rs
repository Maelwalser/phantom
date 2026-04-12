//! `phantom _agent-monitor` — hidden subcommand that monitors a background
//! agent process and runs post-completion automation (submit + materialize).
//!
//! Spawned by `phantom dispatch --background` as a detached process. Waits for
//! the claude process to exit, then submits the changeset and materializes it.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_orchestrator::materializer::MaterializeResult;
use serde::{Deserialize, Serialize};

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct AgentMonitorArgs {
    /// Agent identifier
    #[arg(long)]
    pub agent: String,
    /// Changeset ID for this agent's work
    #[arg(long)]
    pub changeset_id: String,
    /// PID of the claude background process to monitor
    #[arg(long)]
    pub claude_pid: u32,
}

/// Completion status written to `.phantom/overlays/<agent>/agent.status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    /// Exit code of the claude process (None if killed by signal).
    pub exit_code: Option<i32>,
    /// When the agent process completed.
    pub completed_at: chrono::DateTime<Utc>,
    /// Whether the changeset was successfully materialized.
    pub materialized: bool,
    /// Error message if something went wrong during post-completion.
    pub error: Option<String>,
}

/// Path to the agent status file.
pub fn status_path(phantom_dir: &std::path::Path, agent: &str) -> PathBuf {
    phantom_dir.join("overlays").join(agent).join("agent.status")
}

/// Path to the agent PID file.
pub fn pid_path(phantom_dir: &std::path::Path, agent: &str) -> PathBuf {
    phantom_dir.join("overlays").join(agent).join("agent.pid")
}

/// Path to the agent log file.
pub fn log_path(phantom_dir: &std::path::Path, agent: &str) -> PathBuf {
    phantom_dir.join("overlays").join(agent).join("agent.log")
}

/// Path to the monitor PID file.
pub fn monitor_pid_path(phantom_dir: &std::path::Path, agent: &str) -> PathBuf {
    phantom_dir.join("overlays").join(agent).join("monitor.pid")
}

pub async fn run(args: AgentMonitorArgs) -> anyhow::Result<()> {
    // Detach from controlling terminal so we survive parent exit.
    // SAFETY: setsid is always safe to call; it simply creates a new session.
    unsafe {
        libc::setsid();
    }

    let pid = args.claude_pid as libc::pid_t;

    // Wait for the claude process to exit by polling kill(pid, 0).
    // We use polling because the claude process is a child of the dispatch
    // process (not us), so we can't use waitpid directly.
    let exit_code = wait_for_process(pid);

    // Load context and run post-completion flow.
    let agent_id = AgentId(args.agent.clone());
    let changeset_id = ChangesetId(args.changeset_id.clone());

    let result = run_post_completion(&agent_id, &changeset_id, exit_code);

    // Write status file regardless of success/failure.
    // Find .phantom dir by walking up from current directory.
    if let Ok(ctx) = PhantomContext::load() {
        let status = match &result {
            Ok(materialized) => AgentStatus {
                exit_code,
                completed_at: Utc::now(),
                materialized: *materialized,
                error: None,
            },
            Err(e) => AgentStatus {
                exit_code,
                completed_at: Utc::now(),
                materialized: false,
                error: Some(format!("{e:#}")),
            },
        };

        let status_file = status_path(&ctx.phantom_dir, &args.agent);
        if let Ok(json) = serde_json::to_string_pretty(&status) {
            let _ = std::fs::write(&status_file, json);
        }

        // Clean up PID files.
        let _ = std::fs::remove_file(pid_path(&ctx.phantom_dir, &args.agent));
        let _ = std::fs::remove_file(monitor_pid_path(&ctx.phantom_dir, &args.agent));
    }

    result.map(|_| ())
}

/// Poll until the given PID no longer exists. Returns the exit code if
/// we can retrieve it, or None if the process was killed by a signal.
fn wait_for_process(pid: libc::pid_t) -> Option<i32> {
    loop {
        // First try waitpid — works if the process is our child (which it is,
        // since dispatch spawns both claude and the monitor, and the monitor
        // inherits claude as a child after dispatch exits... actually no,
        // claude is a child of dispatch, not us). So we fall back to kill(0).
        let alive = unsafe { libc::kill(pid, 0) };
        if alive != 0 {
            // Process no longer exists. We can't get the exit code via kill().
            // Try waitpid in case it's a zombie we can reap.
            let mut status: libc::c_int = 0;
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if ret > 0 {
                if libc::WIFEXITED(status) {
                    return Some(libc::WEXITSTATUS(status));
                }
                return None; // killed by signal
            }
            // Process is gone and not our child — check if exit code was 0
            // by reading the agent.log for error indicators.
            return None;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Run the post-completion flow: submit + materialize.
/// Returns Ok(true) if materialized, Ok(false) if submitted but not materialized.
fn run_post_completion(
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    exit_code: Option<i32>,
) -> anyhow::Result<bool> {
    let mut ctx = PhantomContext::load()?;

    // Clean up the context file before submitting.
    let upper_dir = ctx
        .overlays
        .upper_dir(agent_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let context_file = upper_dir.join(".phantom-task.md");
    let _ = std::fs::remove_file(&context_file);

    // If the process exited with non-zero, record failure and stop.
    if exit_code != Some(0) {
        let event = Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: changeset_id.clone(),
            agent_id: agent_id.clone(),
            kind: EventKind::AgentCompleted {
                exit_code,
                materialized: false,
            },
        };
        ctx.events
            .append(event)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        anyhow::bail!(
            "background agent exited with code {}",
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        );
    }

    // Auto-submit.
    let submitted_cs_id = super::submit::submit_agent(&ctx, agent_id)
        .context("auto-submit failed")?;

    let materialized = if let Some(cs_id) = submitted_cs_id {
        // Auto-materialize.
        match super::materialize::materialize_changeset(&mut ctx, &cs_id, &agent_id.0)
            .context("auto-materialize failed")?
        {
            MaterializeResult::Success { .. } => true,
            MaterializeResult::Conflict { details } => {
                // Record the completed event with materialized=false.
                let event = Event {
                    id: EventId(0),
                    timestamp: Utc::now(),
                    changeset_id: changeset_id.clone(),
                    agent_id: agent_id.clone(),
                    kind: EventKind::AgentCompleted {
                        exit_code,
                        materialized: false,
                    },
                };
                ctx.events
                    .append(event)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;

                anyhow::bail!(
                    "materialization failed with {} conflict(s): {}",
                    details.len(),
                    details
                        .iter()
                        .map(|d| format!("{}", d.file.display()))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    } else {
        false // no changes to submit
    };

    // Record successful completion.
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::AgentCompleted {
            exit_code,
            materialized,
        },
    };
    ctx.events
        .append(event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(materialized)
}
