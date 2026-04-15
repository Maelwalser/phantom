//! `phantom _agent-monitor` -- hidden subcommand that spawns and monitors a
//! background agent process, then runs post-completion automation (submit,
//! which includes materialization to trunk).
//!
//! Spawned by `phantom <agent> --background`. The monitor is the parent of the
//! claude process so it can `waitpid` to get the real exit code.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_session::adapter;
use phantom_session::context_file;
use phantom_session::post_session::PostSessionOutcome;
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
    /// Path to a system prompt file to append to the claude invocation
    #[arg(long)]
    pub system_prompt_file: Option<String>,
    /// CLI command to use (e.g. "claude", "gemini", "opencode").
    #[arg(long, default_value = "claude")]
    pub cli_command: String,
    /// Comma-separated list of upstream agent IDs that must materialize before
    /// this agent starts. Empty means no dependencies.
    #[arg(long, default_value = "")]
    pub depends_on_agents: String,
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
    phantom_dir
        .join("overlays")
        .join(agent)
        .join("agent.status")
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

    // Wait for upstream dependencies to materialize before starting.
    let upstream_agents: Vec<AgentId> = args
        .depends_on_agents
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| AgentId(s.to_string()))
        .collect();

    if !upstream_agents.is_empty() {
        wait_for_dependencies(&ctx.phantom_dir, &events, &agent_id, &changeset_id, &upstream_agents).await?;

        // Refresh base_commit and context file now that upstream work is on trunk.
        let git = ctx.open_git()?;
        let new_head = git.head_oid().context("failed to read HEAD after deps resolved")?;
        phantom_orchestrator::live_rebase::write_current_base(
            &ctx.phantom_dir,
            &agent_id,
            &new_head,
        )
        .context("failed to update current_base after deps resolved")?;

        // Rewrite the context file with the updated base commit.
        context_file::write_context_file(
            &work_dir,
            &agent_id,
            &changeset_id,
            &new_head,
            Some(&args.task),
        )?;
    }

    // Spawn the agent process as our child so we can waitpid for it.
    let system_prompt_file = args.system_prompt_file.as_deref().map(PathBuf::from);
    let (claude_pid, exit_code) = spawn_and_wait_agent(
        &ctx.phantom_dir,
        &args.agent,
        &work_dir,
        &args.task,
        &args.repo_root,
        &args.cli_command,
        system_prompt_file.as_deref(),
    )?;

    // Emit AgentLaunched event now that we have the real PID.
    let causal_parent = events
        .latest_event_for_changeset(&changeset_id)
        .await
        .unwrap_or(None);
    let launch_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent,
        kind: EventKind::AgentLaunched {
            pid: claude_pid,
            task: args.task.clone(),
        },
    };
    events.append(launch_event).await?;

    // Run post-completion flow: always auto-submit on success.
    let mut result = run_post_completion(&agent_id, &changeset_id, exit_code).await;

    // Auto-retry once on conflict: rebase against current trunk and re-submit.
    // This handles the common case where a parallel agent materialized first and
    // the merge would succeed against the updated trunk.
    if matches!(&result, Ok(PostSessionOutcome::Conflict { .. })) {
        tracing::info!(agent = %agent_id, "conflict detected, retrying with updated trunk base");

        if let Ok(retry_ctx) = PhantomContext::locate()
            && let Ok(git) = retry_ctx.open_git()
            && let Ok(current_head) = git.head_oid()
        {
            // Emit ConflictResolutionStarted with the new base so the
            // submit service uses current trunk HEAD as the merge base.
            let causal = events
                .latest_event_for_changeset(&changeset_id)
                .await
                .unwrap_or(None);
            let resolution_event = Event {
                id: EventId(0),
                timestamp: Utc::now(),
                changeset_id: changeset_id.clone(),
                agent_id: agent_id.clone(),
                causal_parent: causal,
                kind: EventKind::ConflictResolutionStarted {
                    conflicts: vec![],
                    new_base: Some(current_head),
                },
            };
            if events.append(resolution_event).await.is_ok() {
                result =
                    run_post_completion(&agent_id, &changeset_id, exit_code).await;
            }
        }
    }

    // Build status from the outcome.
    let (status, should_destroy) = match &result {
        Ok(outcome) => {
            let materialized = matches!(outcome, PostSessionOutcome::Submitted { .. });
            (
                AgentStatus {
                    exit_code,
                    completed_at: Utc::now(),
                    materialized,
                    error: if matches!(outcome, PostSessionOutcome::Conflict { .. }) {
                        Some("submission failed due to conflicts".into())
                    } else {
                        None
                    },
                },
                materialized, // destroy overlay only on successful materialization
            )
        }
        Err(e) => (
            AgentStatus {
                exit_code,
                completed_at: Utc::now(),
                materialized: false,
                error: Some(format!("{e:#}")),
            },
            false,
        ),
    };

    // Write status file while the overlay still exists.
    let status_file = status_path(&ctx.phantom_dir, &args.agent);
    if let Ok(json) = serde_json::to_string_pretty(&status) {
        let _ = std::fs::write(&status_file, json);
    }

    // Clean up PID files.
    let _ = std::fs::remove_file(pid_path(&ctx.phantom_dir, &args.agent));
    let _ = std::fs::remove_file(monitor_pid_path(&ctx.phantom_dir, &args.agent));

    // Auto-destroy overlay after successful submit. On conflict or failure
    // the overlay is preserved for `phantom resolve` or manual recovery.
    if should_destroy {
        super::destroy::destroy_agent_overlay(&ctx, &agent_id, &changeset_id).await;
    }

    result.map(|_| ())
}

/// Spawn the CLI agent process as our direct child, wait for it, return the exit code.
fn spawn_and_wait_agent(
    phantom_dir: &std::path::Path,
    agent: &str,
    work_dir: &Path,
    task: &str,
    repo_root: &str,
    cli_command: &str,
    system_prompt_file: Option<&Path>,
) -> anyhow::Result<(u32, Option<i32>)> {
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let log_file = overlay_root.join("agent.log");
    let pid_file = overlay_root.join("agent.pid");

    let log_handle = std::fs::File::create(&log_file)
        .with_context(|| format!("failed to create agent log at {}", log_file.display()))?;
    let log_stderr = log_handle
        .try_clone()
        .context("failed to clone log file handle")?;

    let cli_adapter = adapter::adapter_for(cli_command);
    let env_vars: Vec<(&str, &str)> = vec![
        ("PHANTOM_AGENT_ID", agent),
        ("PHANTOM_CHANGESET_ID", ""),
        ("PHANTOM_OVERLAY_DIR", work_dir.to_str().unwrap_or_default()),
        ("PHANTOM_REPO_ROOT", repo_root),
        ("PHANTOM_INTERACTIVE", "0"),
    ];

    let mut cmd = cli_adapter
        .build_headless_command(work_dir, task, &env_vars, system_prompt_file)
        .context("CLI adapter does not support headless mode")?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(log_handle)
        .stderr(log_stderr);

    let mut child = cmd.spawn().with_context(
        || format!("failed to spawn background agent -- is '{cli_command}' installed and on PATH?"),
    )?;

    let claude_pid = child.id();

    // Write PID file so status can find it.
    crate::pid_guard::write_pid_file(&pid_file, claude_pid as i32)
        .context("failed to write agent PID file")?;

    // Wait for the child -- this is our direct child, so waitpid works.
    let status = child
        .wait()
        .context("failed to wait for background agent")?;

    let exit_code = status.code(); // None if killed by signal

    Ok((claude_pid, exit_code))
}

/// Run the post-completion flow: record completion, then auto-submit on
/// success (submit now includes materialization).
async fn run_post_completion(
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    exit_code: Option<i32>,
) -> anyhow::Result<PostSessionOutcome> {
    let ctx = PhantomContext::locate()?;
    let events = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    // Clean up context files (standard + parallel resolve variants).
    let upper_dir = overlays.upper_dir(agent_id)?;
    let context_path = upper_dir.join(context_file::CONTEXT_FILE);
    let _ = std::fs::remove_file(&context_path);
    for i in 0..32 {
        let path = upper_dir.join(format!(".phantom-task-resolve-{i}.md"));
        if !path.exists() {
            break;
        }
        let _ = std::fs::remove_file(&path);
    }

    let success = exit_code == Some(0);

    // Record completion event.
    let causal_parent = events
        .latest_event_for_changeset(changeset_id)
        .await
        .unwrap_or(None);
    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent,
        kind: EventKind::AgentCompleted {
            exit_code,
            materialized: false,
        },
    };
    events.append(event).await?;

    if !success {
        anyhow::bail!(
            "background agent exited with code {}",
            exit_code.map_or_else(|| "signal".into(), |c| c.to_string())
        );
    }

    // Background agents always auto-submit on success (submit includes materialization).
    phantom_session::post_session::post_session_flow(
        phantom_session::post_session::PostSessionContext {
            phantom_dir: &ctx.phantom_dir,
            repo_root: &ctx.repo_root,
            events: &events,
            overlays: &mut overlays,
            agent_id,
            changeset_id,
            auto_submit: true,
        },
    )
    .await
}

/// Status of a single upstream dependency.
#[allow(dead_code)]
enum DepStatus {
    /// The upstream changeset was materialized to trunk.
    Materialized(GitOid),
    /// The upstream is still in progress (no terminal event yet).
    Pending,
    /// The upstream failed (conflicted, dropped, or agent exited non-zero).
    Failed(String),
}

/// Check the status of a single upstream agent by scanning its events.
async fn check_upstream_status(
    events: &SqliteEventStore,
    upstream: &AgentId,
) -> anyhow::Result<DepStatus> {
    let agent_events = events.query_by_agent(upstream).await?;

    // Walk backwards to find the most recent terminal event.
    for event in agent_events.iter().rev() {
        match &event.kind {
            EventKind::ChangesetMaterialized { new_commit } => {
                return Ok(DepStatus::Materialized(*new_commit));
            }
            EventKind::ChangesetConflicted { .. } => {
                return Ok(DepStatus::Failed(format!(
                    "upstream '{upstream}' has merge conflicts"
                )));
            }
            EventKind::ChangesetDropped { reason } => {
                return Ok(DepStatus::Failed(format!(
                    "upstream '{upstream}' was dropped: {reason}"
                )));
            }
            EventKind::AgentCompleted {
                exit_code,
                materialized,
            } => {
                if *exit_code != Some(0) {
                    let code = exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into());
                    return Ok(DepStatus::Failed(format!(
                        "upstream '{upstream}' failed with exit code {code}"
                    )));
                }
                // Agent completed successfully but hasn't materialized yet —
                // materialization event should follow shortly.
                if !materialized {
                    return Ok(DepStatus::Pending);
                }
            }
            _ => {}
        }
    }

    Ok(DepStatus::Pending)
}

/// Wait for all upstream dependencies to materialize to trunk.
///
/// Emits an `AgentWaitingForDependencies` event, then polls the event store
/// until all upstream agents have a `ChangesetMaterialized` event. Bails if
/// any upstream fails.
async fn wait_for_dependencies(
    phantom_dir: &Path,
    events: &SqliteEventStore,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    upstream_agents: &[AgentId],
) -> anyhow::Result<()> {
    // Emit waiting event for observability.
    let causal_parent = events
        .latest_event_for_changeset(changeset_id)
        .await
        .unwrap_or(None);
    let wait_event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        causal_parent,
        kind: EventKind::AgentWaitingForDependencies {
            upstream_agents: upstream_agents.to_vec(),
        },
    };
    events.append(wait_event).await?;

    // Write marker file so `phantom background` / `phantom status` can show
    // the waiting state and the names of upstream agents.
    let waiting_file = phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join("waiting.json");
    let upstream_names: Vec<&str> = upstream_agents.iter().map(|a| a.0.as_str()).collect();
    if let Ok(json) = serde_json::to_string(&upstream_names) {
        let _ = std::fs::write(&waiting_file, json);
    }

    const INITIAL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    const MAX_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
    const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(7200); // 2 hours

    let start = std::time::Instant::now();
    let mut poll_interval = INITIAL_POLL_INTERVAL;

    loop {
        let mut all_satisfied = true;

        for upstream in upstream_agents {
            match check_upstream_status(events, upstream).await? {
                DepStatus::Materialized(_) => {} // satisfied
                DepStatus::Failed(reason) => {
                    anyhow::bail!(
                        "dependency failed, cannot start agent '{agent_id}': {reason}"
                    );
                }
                DepStatus::Pending => {
                    all_satisfied = false;
                }
            }
        }

        if all_satisfied {
            // Remove the waiting marker — agent is about to start.
            let _ = std::fs::remove_file(&waiting_file);
            return Ok(());
        }

        if start.elapsed() > MAX_WAIT {
            let pending: Vec<&str> = upstream_agents.iter().map(|a| a.0.as_str()).collect();
            anyhow::bail!(
                "timed out waiting for upstream dependencies: {}",
                pending.join(", ")
            );
        }

        tokio::time::sleep(poll_interval).await;
        poll_interval = poll_interval.mul_f32(1.5).min(MAX_POLL_INTERVAL);
    }
}
