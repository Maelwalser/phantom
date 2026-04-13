//! `phantom _agent-monitor` -- hidden subcommand that spawns and monitors a
//! background agent process, then runs post-completion automation (submit +
//! materialize).
//!
//! Spawned by `phantom task --background`. The monitor is the parent of the
//! claude process so it can `waitpid` to get the real exit code.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_session::adapter;
use phantom_session::context_file;
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
    /// Task description to pass to the claude process
    #[arg(long)]
    pub task: String,
    /// Working directory for the claude process
    #[arg(long)]
    pub work_dir: String,
    /// Repository root
    #[arg(long)]
    pub repo_root: String,
    /// Automatically materialize after submitting
    #[arg(long)]
    pub auto_materialize: bool,
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

    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let agent_id = AgentId(args.agent.clone());
    let changeset_id = ChangesetId(args.changeset_id.clone());
    let work_dir = PathBuf::from(&args.work_dir);

    // Spawn the claude process as our child so we can waitpid for it.
    let (claude_pid, exit_code) =
        spawn_and_wait_claude(&ctx.phantom_dir, &args.agent, &work_dir, &args.task, &args.repo_root)?;

    // Emit AgentLaunched event now that we have the real PID.
    let launch_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::AgentLaunched {
            pid: claude_pid,
            task: args.task.clone(),
        },
    };
    events
        .append(launch_event)
        .await
        ?;

    // Run post-completion flow: always auto-submit on success, optionally auto-materialize.
    let result = run_post_completion(
        &agent_id,
        &changeset_id,
        exit_code,
        args.auto_materialize,
    )
    .await;

    // Write status file regardless of success/failure.
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

    result.map(|_| ())
}

/// Spawn the claude process as our direct child, wait for it, return the exit code.
fn spawn_and_wait_claude(
    phantom_dir: &std::path::Path,
    agent: &str,
    work_dir: &Path,
    task: &str,
    repo_root: &str,
) -> anyhow::Result<(u32, Option<i32>)> {
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let log_file = overlay_root.join("agent.log");
    let pid_file = overlay_root.join("agent.pid");

    let log_handle = std::fs::File::create(&log_file)
        .with_context(|| format!("failed to create agent log at {}", log_file.display()))?;
    let log_stderr = log_handle
        .try_clone()
        .context("failed to clone log file handle")?;

    let cli_adapter = adapter::adapter_for("claude");
    let env_vars: Vec<(&str, &str)> = vec![
        ("PHANTOM_AGENT_ID", agent),
        ("PHANTOM_CHANGESET_ID", ""),
        ("PHANTOM_OVERLAY_DIR", work_dir.to_str().unwrap_or_default()),
        ("PHANTOM_REPO_ROOT", repo_root),
        ("PHANTOM_INTERACTIVE", "0"),
    ];

    let mut cmd = cli_adapter
        .build_headless_command(work_dir, task, &env_vars)
        .context("CLI adapter does not support headless mode")?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(log_handle)
        .stderr(log_stderr);

    let mut child = cmd.spawn().with_context(|| {
        "failed to spawn background agent -- is 'claude' installed and on PATH?"
    })?;

    let claude_pid = child.id();

    // Write PID file so status can find it.
    std::fs::write(&pid_file, claude_pid.to_string())
        .context("failed to write agent PID file")?;

    // Wait for the child -- this is our direct child, so waitpid works.
    let status = child
        .wait()
        .context("failed to wait for background agent")?;

    let exit_code = status.code(); // None if killed by signal

    Ok((claude_pid, exit_code))
}

/// Run the post-completion flow: record completion, then auto-submit (and
/// optionally auto-materialize) on success.
///
/// Returns `Ok(true)` if the changeset was materialized, `Ok(false)` if it was
/// only submitted (or the agent had no changes).
async fn run_post_completion(
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    exit_code: Option<i32>,
    auto_materialize: bool,
) -> anyhow::Result<bool> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    // Clean up the context file.
    let upper_dir = overlays
        .upper_dir(agent_id)
        ?;
    let context_path = upper_dir.join(context_file::CONTEXT_FILE);
    let _ = std::fs::remove_file(&context_path);

    let success = exit_code == Some(0);

    // Record completion event.
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
    events
        .append(event)
        .await
        ?;

    if !success {
        anyhow::bail!(
            "background agent exited with code {}",
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        );
    }

    // Background agents always auto-submit on success.
    phantom_session::post_session::post_session_flow(
        &ctx.phantom_dir,
        &ctx.repo_root,
        &events,
        &mut overlays,
        agent_id,
        changeset_id,
        true, // auto_submit — always true for background agents
        auto_materialize,
    )
    .await?;

    Ok(auto_materialize)
}
