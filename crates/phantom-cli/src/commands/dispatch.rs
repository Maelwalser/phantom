//! `phantom dispatch` — create an agent overlay and launch a Claude Code session.
//!
//! By default, dispatching opens an interactive Claude Code CLI inside the
//! overlay's FUSE mount point, which provides a merged view of trunk + agent
//! writes. Use `--background` to create the overlay without launching a
//! session (for scripted / headless agents).

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, EventId};
use phantom_core::traits::EventStore;

use crate::context::PhantomContext;

#[derive(clap::Args)]
pub struct DispatchArgs {
    /// Agent identifier (e.g. "agent-a")
    pub agent: String,
    /// Task description for the agent (only available with --background)
    #[arg(long, requires = "background")]
    pub task: Option<String>,
    /// Create the overlay without launching a CLI session (for scripted agents)
    #[arg(long, short = 'b', requires = "task")]
    pub background: bool,
    /// Automatically submit the changeset when the interactive session exits
    #[arg(long, conflicts_with = "background")]
    pub auto_submit: bool,
    /// Automatically materialize after submitting (implies --auto-submit)
    #[arg(long, conflicts_with = "background")]
    pub auto_materialize: bool,
    /// Custom command to run instead of `claude` (e.g. for testing)
    #[arg(long, conflicts_with = "background")]
    pub command: Option<String>,
    /// Skip FUSE mounting (agent works via OverlayLayer API only, no filesystem isolation)
    #[arg(long)]
    pub no_fuse: bool,
}

pub async fn run(args: DispatchArgs) -> anyhow::Result<()> {
    let mut ctx = PhantomContext::load()?;

    let agent_id = AgentId(args.agent.clone());
    let head = ctx.git.head_oid().context("failed to read HEAD")?;

    let changeset_id = generate_changeset_id(&ctx)?;

    let handle = ctx
        .overlays
        .create_overlay(agent_id.clone(), &ctx.repo_root)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let mount_point = handle.mount_point.clone();
    let upper_dir = handle.upper_dir.clone();

    let event = Event {
        id: EventId(0),
        timestamp: Utc::now(),
        changeset_id: changeset_id.clone(),
        agent_id: agent_id.clone(),
        kind: EventKind::OverlayCreated {
            base_commit: head,
            task: args.task.clone().unwrap_or_default(),
        },
    };
    ctx.events
        .append(event)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Spawn FUSE daemon unless --no-fuse
    let fuse_mounted = if args.no_fuse {
        false
    } else {
        spawn_fuse_daemon(&ctx, &args.agent, &mount_point, &upper_dir)?
    };

    // The agent's working directory: FUSE mount (merged view) or upper dir (writes only).
    let work_dir = if fuse_mounted {
        mount_point.clone()
    } else {
        upper_dir.clone()
    };

    let base_short = head.to_hex().chars().take(12).collect::<String>();

    if args.background {
        let task = args.task.as_deref().unwrap_or("");

        super::interactive::write_context_file(
            &work_dir,
            &agent_id,
            &changeset_id,
            &head,
            Some(task),
        )?;

        println!("Agent '{}' dispatched (background).", args.agent);
        println!("  Changeset: {changeset_id}");
        println!("  Task:      {task}");
        println!("  Overlay:   {}", work_dir.display());
        println!("  Base:      {base_short}");
        if fuse_mounted {
            println!("  FUSE:      mounted");
        }
    } else {
        println!("Agent '{}' dispatched.", args.agent);
        println!("  Changeset: {changeset_id}");
        println!("  Overlay:   {}", work_dir.display());
        println!("  Base:      {base_short}");
        if fuse_mounted {
            println!("  FUSE:      mounted");
        }
        println!();
        super::interactive::run_interactive_session(
            &mut ctx,
            &agent_id,
            &changeset_id,
            &head,
            &work_dir,
            &args,
        )?;
    }

    Ok(())
}

/// Spawn a FUSE daemon process that mounts `PhantomFs` at the overlay's mount point.
///
/// Returns `true` if the mount was successful, `false` if FUSE is unavailable.
fn spawn_fuse_daemon(
    ctx: &PhantomContext,
    agent: &str,
    mount_point: &Path,
    upper_dir: &Path,
) -> anyhow::Result<bool> {
    let phantom_bin = std::env::current_exe().context("failed to find phantom binary")?;
    let overlay_root = ctx.phantom_dir.join("overlays").join(agent);
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
        .arg(&ctx.repo_root)
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
        if let (Ok(m), Ok(p)) = (std::fs::metadata(mount_point), std::fs::metadata(parent)) {
            if m.dev() != p.dev() {
                return Ok(());
            }
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
/// suffix to avoid collisions from concurrent dispatch calls.
fn generate_changeset_id(ctx: &PhantomContext) -> anyhow::Result<ChangesetId> {
    let events = ctx.events.query_all().map_err(|e| anyhow::anyhow!("{e}"))?;

    let overlay_count = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::OverlayCreated { .. }))
        .count();

    // Append timestamp micros to avoid race condition when two dispatches
    // read the same count concurrently.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        % 1_000_000;

    Ok(ChangesetId(format!("cs-{:04}-{ts:06}", overlay_count + 1)))
}
