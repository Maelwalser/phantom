//! `phantom <agent>` — create an agent overlay and launch a Claude Code session.
//!
//! By default, tasking opens an interactive Claude Code CLI inside the
//! overlay's FUSE mount point, which provides a merged view of trunk + agent
//! writes. Use `--background` to create the overlay without launching a
//! session (for scripted / headless agents).
//!
//! If an overlay already exists for the agent, the command resumes the existing
//! session (reuses changeset ID, skips event emission, re-mounts FUSE if needed).

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId, GitOid};
use phantom_core::traits::EventStore;
use phantom_events::SqliteEventStore;
use phantom_overlay::OverlayError;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct TaskArgs {
    /// Agent identifier (e.g. "agent-a")
    pub agent: String,
    /// Task description for the agent (only available with --background)
    #[arg(long, requires = "background")]
    pub task: Option<String>,
    /// Create the overlay without launching a CLI session (for scripted agents)
    #[arg(long, short = 'b', requires = "task")]
    pub background: bool,
    /// Automatically submit the changeset when the session exits.
    /// Always enabled for background agents.
    #[arg(long)]
    pub auto_submit: bool,
    /// Automatically materialize after submitting (implies --auto-submit)
    #[arg(long)]
    pub auto_materialize: bool,
    /// Custom command to run instead of `claude` (e.g. for testing)
    #[arg(long, conflicts_with = "background")]
    pub command: Option<String>,
    /// Skip FUSE mounting (agent works via OverlayLayer API only, no filesystem isolation)
    #[arg(long)]
    pub no_fuse: bool,
}

pub async fn run(args: TaskArgs) -> anyhow::Result<()> {
    let ctx = PhantomContext::locate()?;
    let git = ctx.open_git()?;
    let events = ctx.open_events().await?;
    let mut overlays = ctx.open_overlays_restored()?;

    let agent_id = AgentId(args.agent.clone());
    let head = git.head_oid().context("failed to read HEAD")?;

    // Try to create a new overlay. If one already exists, switch to resume mode.
    let (changeset_id, base_commit, is_new, mount_point, upper_dir) =
        match overlays.create_overlay(agent_id.clone(), &ctx.repo_root) {
            Ok(handle) => {
                let mount_point = handle.mount_point.clone();
                let upper_dir = handle.upper_dir.clone();

                let cs_id = generate_changeset_id(&events).await?;

                let event = Event {
                    id: EventId(0),
                    timestamp: Utc::now(),
                    changeset_id: cs_id.clone(),
                    agent_id: agent_id.clone(),
                    kind: EventKind::TaskCreated {
                        base_commit: head,
                        task: args.task.clone().unwrap_or_default(),
                    },
                };
                events
                    .append(event)
                    .await
                    ?;

                phantom_orchestrator::live_rebase::write_current_base(
                    &ctx.phantom_dir,
                    &agent_id,
                    &head,
                )
                .context("failed to write initial current_base")?;

                (cs_id, head, true, mount_point, upper_dir)
            }
            Err(OverlayError::AlreadyExists(_)) => {
                let (old_cs_id, old_base) = recover_changeset_from_events(&events, &agent_id).await?;
                let resume_status = check_changeset_resumable(&events, &old_cs_id).await?;

                let upper_dir = overlays
                    .upper_dir(&agent_id)
                    ?
                    .to_path_buf();
                let mount_point = ctx
                    .phantom_dir
                    .join("overlays")
                    .join(&agent_id.0)
                    .join("mount");

                // If the previous changeset was materialized, start a new one
                // so the agent can continue working on the same overlay.
                let (cs_id, base) = match resume_status {
                    ResumeStatus::Materialized => {
                        let new_cs_id = generate_changeset_id(&events).await?;
                        let event = Event {
                            id: EventId(0),
                            timestamp: Utc::now(),
                            changeset_id: new_cs_id.clone(),
                            agent_id: agent_id.clone(),
                            kind: EventKind::TaskCreated {
                                base_commit: head,
                                task: args.task.clone().unwrap_or_default(),
                            },
                        };
                        events.append(event).await?;

                        phantom_orchestrator::live_rebase::write_current_base(
                            &ctx.phantom_dir,
                            &agent_id,
                            &head,
                        )
                        .context("failed to write current_base for new changeset")?;

                        (new_cs_id, head)
                    }
                    ResumeStatus::Submitted | ResumeStatus::InProgress => {
                        (old_cs_id, old_base)
                    }
                };

                (cs_id, base, false, mount_point, upper_dir)
            }
            Err(e) => return Err(e.into()),
        };

    // Spawn FUSE daemon unless --no-fuse or already mounted.
    let already_mounted = is_fuse_mounted(&mount_point);
    let fuse_mounted = if args.no_fuse || already_mounted {
        already_mounted
    } else {
        spawn_fuse_daemon(&ctx.phantom_dir, &ctx.repo_root, &args.agent, &mount_point, &upper_dir)?
    };

    // The agent's working directory: FUSE mount (merged view) or upper dir (writes only).
    let work_dir = if fuse_mounted {
        mount_point.clone()
    } else {
        upper_dir.clone()
    };

    let base_short = base_commit.to_hex().chars().take(12).collect::<String>();
    let verb = if is_new { "tasked" } else { "resumed" };

    if args.background {
        let task = args.task.as_deref().unwrap_or("");

        super::interactive::write_context_file(
            &work_dir,
            &agent_id,
            &changeset_id,
            &base_commit,
            Some(task),
        )?;

        // Spawn the monitor process, which in turn spawns claude as its child.
        // This ensures the monitor can waitpid for the real exit code.
        let log_file = ctx.phantom_dir.join("overlays").join(&args.agent).join("agent.log");
        spawn_agent_monitor(
            &ctx.phantom_dir,
            &ctx.repo_root,
            &args.agent,
            &changeset_id,
            task,
            &work_dir,
            args.auto_materialize,
        )?;

        println!("Agent '{}' {verb} (background).", args.agent);
        println!("  Changeset: {changeset_id}");
        println!("  Task:      {task}");
        println!("  Log:       {}", log_file.display());
        println!("  Overlay:   {}", work_dir.display());
        println!("  Base:      {base_short}");
        if fuse_mounted {
            println!("  FUSE:      mounted");
        }
        println!();
        println!("Run `phantom {}` again to check progress.", args.agent);
    } else {
        // If a background agent is already running or has completed for this
        // overlay, show its status instead of opening an interactive session.
        // This prevents accidentally launching a second CLI on top of a
        // background agent's work.
        if !is_new && has_background_agent(&ctx.phantom_dir, &args.agent) {
            // Delegate to the detailed status view.
            super::status::run(super::status::StatusArgs {
                agent: Some(args.agent.clone()),
            })
            .await?;
            return Ok(());
        }

        if is_new {
            println!("Agent '{}' tasked.", args.agent);
            println!("  Changeset: {changeset_id}");
            println!("  Overlay:   {}", work_dir.display());
            println!("  Base:      {base_short}");
            if fuse_mounted {
                println!("  FUSE:      mounted");
            }
            println!();
        } else {
            println!("Task '{}' resumed.", args.agent);
        }
        super::interactive::run_interactive_session(
            &ctx,
            &agent_id,
            &changeset_id,
            &base_commit,
            &work_dir,
            &args,
        )
        .await?;
    }

    Ok(())
}

/// Spawn a FUSE daemon process that mounts `PhantomFs` at the overlay's mount point.
///
/// Returns `true` if the mount was successful, `false` if FUSE is unavailable.
fn spawn_fuse_daemon(
    phantom_dir: &Path,
    repo_root: &Path,
    agent: &str,
    mount_point: &Path,
    upper_dir: &Path,
) -> anyhow::Result<bool> {
    let phantom_bin = std::env::current_exe().context("failed to find phantom binary")?;
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let pid_file = overlay_root.join("fuse.pid");
    let log_file = overlay_root.join("fuse.log");

    let log_handle = std::fs::File::create(&log_file)
        .with_context(|| format!("failed to create FUSE log at {}", log_file.display()))?;

    let child = std::process::Command::new(&phantom_bin)
        .arg("_fuse-mount")
        .arg("--agent")
        .arg(agent)
        .arg("--mount-point")
        .arg(mount_point)
        .arg("--upper-dir")
        .arg(upper_dir)
        .arg("--lower-dir")
        .arg(repo_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log_handle)
        .spawn()
        .context("failed to spawn FUSE daemon")?;

    std::fs::write(&pid_file, child.id().to_string())
        .context("failed to write FUSE PID file")?;

    wait_for_mount(mount_point, Duration::from_secs(5)).with_context(|| {
        format!(
            "FUSE mount did not become ready. Check {}",
            log_file.display()
        )
    })?;

    Ok(true)
}

/// Poll until a FUSE mount appears at `mount_point`.
///
/// Detects the mount by comparing the device ID of the mount point to its
/// parent directory — a mounted filesystem has a different device.
fn wait_for_mount(mount_point: &Path, timeout: Duration) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let parent = mount_point
        .parent()
        .context("mount point has no parent directory")?;
    let start = std::time::Instant::now();

    loop {
        if let (Ok(m), Ok(p)) = (std::fs::metadata(mount_point), std::fs::metadata(parent))
            && m.dev() != p.dev() {
                return Ok(());
            }

        if start.elapsed() > timeout {
            anyhow::bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Generate a unique changeset ID.
///
/// Uses a monotonic counter from the event store combined with a timestamp
/// suffix to avoid collisions from concurrent task calls.
async fn generate_changeset_id(events: &SqliteEventStore) -> anyhow::Result<ChangesetId> {
    let events = events.query_all().await?;

    let overlay_count = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::TaskCreated { .. }))
        .count();

    // Append timestamp micros to avoid race condition when two task calls
    // read the same count concurrently.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        % 1_000_000;

    Ok(ChangesetId(format!("cs-{:04}-{ts:06}", overlay_count + 1)))
}

/// Recover the changeset ID and base commit for an existing agent overlay from
/// the event log. Finds the most recent `TaskCreated` event for this agent.
async fn recover_changeset_from_events(
    events: &SqliteEventStore,
    agent_id: &AgentId,
) -> anyhow::Result<(ChangesetId, GitOid)> {
    let events = events
        .query_by_agent(agent_id)
        .await
        ?;

    // Walk backwards to find the most recent TaskCreated event.
    for event in events.iter().rev() {
        if let EventKind::TaskCreated { base_commit, .. } = &event.kind {
            return Ok((event.changeset_id.clone(), *base_commit));
        }
    }

    anyhow::bail!(
        "overlay exists for agent '{}' but no TaskCreated event found in the event log",
        agent_id
    )
}

/// Status of a changeset when attempting to resume a session.
enum ResumeStatus {
    /// The changeset is still in-progress — resume normally.
    InProgress,
    /// The changeset was submitted but not yet materialized — resume with the
    /// same changeset (agent can continue editing and re-submit).
    Submitted,
    /// The changeset was materialized — a new changeset should be created for
    /// continued work on this overlay.
    Materialized,
}

/// Check the resume status of a changeset.
///
/// Blocks resume only if the task has been explicitly destroyed. Otherwise
/// returns the changeset status so the caller can decide whether to reuse the
/// changeset or create a new one.
async fn check_changeset_resumable(events: &SqliteEventStore, cs_id: &ChangesetId) -> anyhow::Result<ResumeStatus> {
    let events = events
        .query_by_changeset(cs_id)
        .await
        ?;

    let mut materialized = false;
    let mut submitted = false;

    for event in &events {
        match &event.kind {
            EventKind::TaskDestroyed => {
                anyhow::bail!(
                    "task for changeset {cs_id} has been destroyed — \
                     use `phantom <new-agent>` to start fresh"
                );
            }
            EventKind::ChangesetMaterialized { .. } => {
                materialized = true;
            }
            EventKind::ChangesetSubmitted { .. } => {
                submitted = true;
            }
            _ => {}
        }
    }

    if materialized {
        Ok(ResumeStatus::Materialized)
    } else if submitted {
        Ok(ResumeStatus::Submitted)
    } else {
        Ok(ResumeStatus::InProgress)
    }
}

/// Spawn the `phantom _agent-monitor` process which will in turn spawn and
/// monitor the claude process. This ensures the monitor is the parent of
/// claude and can `waitpid` to get the real exit code.
fn spawn_agent_monitor(
    phantom_dir: &Path,
    repo_root: &Path,
    agent: &str,
    changeset_id: &ChangesetId,
    task: &str,
    work_dir: &Path,
    auto_materialize: bool,
) -> anyhow::Result<()> {
    let phantom_bin = std::env::current_exe().context("failed to find phantom binary")?;
    let overlay_root = phantom_dir.join("overlays").join(agent);
    let monitor_pid_file = overlay_root.join("monitor.pid");

    let mut cmd = std::process::Command::new(&phantom_bin);
    cmd.arg("_agent-monitor")
        .arg("--agent")
        .arg(agent)
        .arg("--changeset-id")
        .arg(&changeset_id.0)
        .arg("--task")
        .arg(task)
        .arg("--work-dir")
        .arg(work_dir.as_os_str())
        .arg("--repo-root")
        .arg(repo_root);

    if auto_materialize {
        cmd.arg("--auto-materialize");
    }

    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn agent monitor")?;

    std::fs::write(&monitor_pid_file, child.id().to_string())
        .context("failed to write monitor PID file")?;

    Ok(())
}

/// Check if a background agent process exists for this agent — either still
/// running (agent.pid with a live process) or finished (agent.status exists).
fn has_background_agent(phantom_dir: &Path, agent: &str) -> bool {
    let overlay_dir = phantom_dir.join("overlays").join(agent);

    // A completion marker means a background agent ran.
    if overlay_dir.join("agent.status").exists() {
        return true;
    }

    // A PID file means a background agent was launched (may still be running).
    if overlay_dir.join("agent.pid").exists() {
        return true;
    }

    false
}

/// Check if a FUSE filesystem is already mounted at `mount_point` by comparing
/// its device ID to its parent directory.
fn is_fuse_mounted(mount_point: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let parent = match mount_point.parent() {
        Some(p) => p,
        None => return false,
    };

    match (std::fs::metadata(mount_point), std::fs::metadata(parent)) {
        (Ok(m), Ok(p)) => m.dev() != p.dev(),
        _ => false,
    }
}
