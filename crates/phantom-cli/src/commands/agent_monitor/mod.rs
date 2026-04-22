//! `phantom _agent-monitor` -- hidden subcommand that spawns and monitors a
//! background agent process, then runs post-completion automation (submit,
//! which includes materialization to trunk).
//!
//! Spawned by `phantom <agent> --background`. The monitor is the parent of the
//! claude process so it can `waitpid` to get the real exit code.

mod deps;
mod retry;
mod status;

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_events::query::EventQuery;
use phantom_session::adapter;
use phantom_session::context_file;
use phantom_session::post_session::PostSessionOutcome;

use crate::context::PhantomContext;

pub use status::AgentStatus;

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

/// Path to the agent status file.
pub fn status_path(phantom_dir: &Path, agent: &str) -> PathBuf {
    phantom_dir
        .join("overlays")
        .join(agent)
        .join("agent.status")
}

/// Path to the agent PID file.
pub fn pid_path(phantom_dir: &Path, agent: &str) -> PathBuf {
    phantom_dir.join("overlays").join(agent).join("agent.pid")
}

/// Path to the agent log file.
pub fn log_path(phantom_dir: &Path, agent: &str) -> PathBuf {
    phantom_dir.join("overlays").join(agent).join("agent.log")
}

/// Path to the monitor PID file.
pub fn monitor_pid_path(phantom_dir: &Path, agent: &str) -> PathBuf {
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
        deps::wait_for_dependencies(
            &ctx.phantom_dir,
            &events,
            &agent_id,
            &changeset_id,
            &upstream_agents,
        )
        .await?;

        // Refresh base_commit and context file now that upstream work is on trunk.
        let git = ctx.open_git()?;
        let new_head = git
            .head_oid()
            .context("failed to read HEAD after deps resolved")?;
        phantom_orchestrator::live_rebase::write_current_base(
            &ctx.phantom_dir,
            &agent_id,
            &new_head,
        )
        .context("failed to update current_base after deps resolved")?;

        // Rewrite the context file with the updated base commit. Re-detecting
        // the toolchain is cheap (stat-only, stateless) and keeps the
        // verification block consistent with the freshly-materialised trunk.
        let toolchain =
            phantom_toolchain::ToolchainDetector::new().detect_repo_root(&ctx.repo_root);
        context_file::write_context_file_with_toolchain(
            &work_dir,
            &agent_id,
            &changeset_id,
            &new_head,
            Some(&args.task),
            Some(&toolchain),
        )?;
    }

    // Write the per-overlay Claude settings so the background agent drains
    // trunk-update notifications automatically. Canonical copy lands at
    // `<work_dir>/.claude/settings.json` (the path Claude trusts), marker
    // copy at `.phantom/overlays/<agent>/claude-settings.json`.
    // Failure here is non-fatal — we fall back to file-only delivery.
    if let Err(e) = phantom_session::hook_config::write(&ctx.phantom_dir, &agent_id, &work_dir) {
        tracing::warn!(error = %e, "failed to write hook settings; falling back to file-only delivery");
    }
    let marker = phantom_session::hook_config::settings_path(&ctx.phantom_dir, &agent_id);
    let hook_settings_file = if marker.exists() { Some(marker) } else { None };

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
        hook_settings_file.as_deref(),
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

    // Run post-completion flow, with an auto-retry if the first attempt hits
    // a conflict (another agent materialized first — rebase + retry).
    let initial = run_post_completion(&agent_id, &changeset_id, exit_code).await;
    let result =
        retry::maybe_retry_on_conflict(&events, &agent_id, &changeset_id, exit_code, initial).await;

    // Build status from the outcome.
    let (status, should_remove) = match &result {
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
                materialized, // remove overlay only on successful materialization
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

    // Reconcile against the event log. The monitor's local result can end in
    // an error even though the work already made it to trunk — e.g. the
    // submit pipeline crashed after `ChangesetMaterialized` but before
    // `ChangesetSubmitted` was appended, or the conflict-retry path tripped
    // on a non-fatal error. The event log is the source of truth: if trunk
    // holds the commit, the agent succeeded and we should not plant a
    // failure marker.
    let (status, should_remove) =
        reconcile_with_event_log(&events, &agent_id, &changeset_id, status, should_remove).await;

    // Write status file while the overlay still exists.
    let status_file = status_path(&ctx.phantom_dir, &args.agent);
    if let Ok(json) = serde_json::to_string_pretty(&status) {
        let _ = std::fs::write(&status_file, json);
    }

    // Clean up PID files.
    let _ = std::fs::remove_file(pid_path(&ctx.phantom_dir, &args.agent));
    let _ = std::fs::remove_file(monitor_pid_path(&ctx.phantom_dir, &args.agent));

    // Auto-remove overlay after successful submit. On conflict or failure
    // the overlay is preserved for `phantom resolve` or manual recovery.
    if should_remove {
        super::remove::remove_agent_overlay(&ctx, &agent_id, &changeset_id).await;
    }

    result.map(|_| ())
}

/// If the local result would mark the agent as failed but the event log shows
/// `ChangesetMaterialized` for this changeset, override the status to success.
///
/// Prevents a false "failed" marker from a mid-pipeline error (e.g., a failed
/// `ChangesetSubmitted` append, or a retry path that tripped on a secondary
/// error) after the work already landed on trunk.
async fn reconcile_with_event_log(
    events: &SqliteEventStore,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    status: AgentStatus,
    should_remove: bool,
) -> (AgentStatus, bool) {
    let needs_check = status.error.is_some() || status.exit_code != Some(0);
    if !needs_check {
        return (status, should_remove);
    }

    if !was_changeset_materialized(events, changeset_id).await {
        return (status, should_remove);
    }

    tracing::warn!(
        agent = %agent_id,
        changeset = %changeset_id,
        original_error = ?status.error,
        original_exit_code = ?status.exit_code,
        "reconciling agent.status: ChangesetMaterialized event exists for this changeset — overriding to success"
    );

    (
        AgentStatus {
            exit_code: Some(0),
            completed_at: status.completed_at,
            materialized: true,
            error: None,
        },
        true,
    )
}

/// Return `true` if a `ChangesetMaterialized` event has been recorded for the
/// given changeset. Errors from the event store are treated as "not found" so
/// that a transient query failure does not accidentally promote a real error
/// status into a fake success.
async fn was_changeset_materialized(events: &SqliteEventStore, changeset_id: &ChangesetId) -> bool {
    let query = EventQuery {
        changeset_id: Some(changeset_id.clone()),
        kind_prefixes: vec!["ChangesetMaterialized".into()],
        limit: Some(1),
        ..Default::default()
    };
    events.query(&query).await.is_ok_and(|evs| !evs.is_empty())
}

/// Spawn the CLI agent process as our direct child, wait for it, return the exit code.
#[allow(clippy::too_many_arguments)] // all args are orthogonal and a struct
// would just shuffle the same fields around the call site
fn spawn_and_wait_agent(
    phantom_dir: &Path,
    agent: &str,
    work_dir: &Path,
    task: &str,
    repo_root: &str,
    cli_command: &str,
    system_prompt_file: Option<&Path>,
    hook_settings_file: Option<&Path>,
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
        .build_headless_command(
            work_dir,
            task,
            &env_vars,
            system_prompt_file,
            hook_settings_file,
        )
        .context("CLI adapter does not support headless mode")?;

    cmd.stdin(std::process::Stdio::null())
        .stdout(log_handle)
        .stderr(log_stderr);

    let mut child = cmd.spawn().with_context(|| {
        format!("failed to spawn background agent -- is '{cli_command}' installed and on PATH?")
    })?;

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
pub(super) async fn run_post_completion(
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

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::changeset::SemanticOperation;
    use phantom_core::id::GitOid;

    async fn store_with_materialized(cs_id: &ChangesetId, agent_id: &AgentId) -> SqliteEventStore {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let event = Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: cs_id.clone(),
            agent_id: agent_id.clone(),
            causal_parent: None,
            kind: EventKind::ChangesetMaterialized {
                new_commit: GitOid::zero(),
            },
        };
        store.append(event).await.unwrap();
        store
    }

    fn failing_status() -> AgentStatus {
        AgentStatus {
            exit_code: Some(0),
            completed_at: Utc::now(),
            materialized: false,
            error: Some("submission failed due to conflicts".into()),
        }
    }

    #[tokio::test]
    async fn reconcile_overrides_error_status_when_materialized_event_exists() {
        let agent = AgentId("scribe".into());
        let cs = ChangesetId("cs-1".into());
        let events = store_with_materialized(&cs, &agent).await;

        let (status, should_remove) =
            reconcile_with_event_log(&events, &agent, &cs, failing_status(), false).await;

        assert!(status.error.is_none(), "error should be cleared");
        assert_eq!(status.exit_code, Some(0));
        assert!(status.materialized, "materialized should be true");
        assert!(
            should_remove,
            "overlay should be removed on reconciled success"
        );
    }

    #[tokio::test]
    async fn reconcile_preserves_error_when_no_materialized_event() {
        let agent = AgentId("scribe".into());
        let cs = ChangesetId("cs-1".into());
        let events = SqliteEventStore::in_memory().await.unwrap();
        // Append an unrelated event so the store isn't empty.
        let unrelated = Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: cs.clone(),
            agent_id: agent.clone(),
            causal_parent: None,
            kind: EventKind::ChangesetSubmitted {
                operations: Vec::<SemanticOperation>::new(),
            },
        };
        events.append(unrelated).await.unwrap();

        let (status, should_remove) =
            reconcile_with_event_log(&events, &agent, &cs, failing_status(), false).await;

        assert!(status.error.is_some(), "error should be preserved");
        assert!(!status.materialized);
        assert!(!should_remove);
    }

    #[tokio::test]
    async fn reconcile_is_noop_for_already_successful_status() {
        let agent = AgentId("scribe".into());
        let cs = ChangesetId("cs-1".into());
        // Even with a materialized event present, a successful input should
        // pass through unchanged (no unnecessary query).
        let events = store_with_materialized(&cs, &agent).await;
        let success = AgentStatus {
            exit_code: Some(0),
            completed_at: Utc::now(),
            materialized: true,
            error: None,
        };

        let (status, should_remove) =
            reconcile_with_event_log(&events, &agent, &cs, success.clone(), true).await;

        assert!(status.error.is_none());
        assert!(status.materialized);
        assert!(should_remove);
    }
}
