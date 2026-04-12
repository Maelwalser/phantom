//! Shared context loaded by each command.
//!
//! [`PhantomContext`] locates the `.phantom/` directory, opens the event store,
//! overlay manager, git handle, and semantic merger so individual commands can
//! focus on their logic.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use phantom_events::SqliteEventStore;
use phantom_orchestrator::git::GitOps;
use phantom_overlay::OverlayManager;
use phantom_semantic::SemanticMerger;
use tracing::warn;

/// Everything a command needs to interact with a Phantom-managed repository.
#[allow(dead_code)]
pub struct PhantomContext {
    pub phantom_dir: PathBuf,
    pub repo_root: PathBuf,
    pub git: GitOps,
    pub events: SqliteEventStore,
    pub overlays: OverlayManager,
    pub semantic: SemanticMerger,
}

impl PhantomContext {
    /// Find `.phantom/` by walking up from the current directory. Open all subsystems.
    pub fn load() -> anyhow::Result<Self> {
        let cwd = std::env::current_dir().context("failed to determine current directory")?;
        let phantom_dir = find_phantom_dir(&cwd)?;
        let repo_root = phantom_dir
            .parent()
            .expect(".phantom/ must have a parent")
            .to_path_buf();

        let git = GitOps::open(&repo_root).context("failed to open git repository")?;

        let events_path = phantom_dir.join("events.db");
        let events = SqliteEventStore::open(&events_path).context("failed to open event store")?;

        let mut overlays = OverlayManager::new(phantom_dir.clone());
        let semantic = SemanticMerger::new();

        // Restore overlay state from disk — scan .phantom/overlays/ for existing
        // agent directories that have an "upper" subdirectory.
        restore_overlays(&mut overlays, &phantom_dir, &repo_root)?;

        Ok(Self {
            phantom_dir,
            repo_root,
            git,
            events,
            overlays,
            semantic,
        })
    }
}

/// Scan `.phantom/overlays/` for existing agent directories and re-register them
/// with the overlay manager so they survive across CLI invocations.
///
/// Also cleans up stale FUSE mounts where the daemon died but the PID file
/// remains.
fn restore_overlays(
    overlays: &mut OverlayManager,
    phantom_dir: &Path,
    repo_root: &Path,
) -> anyhow::Result<()> {
    let overlays_dir = phantom_dir.join("overlays");
    if !overlays_dir.is_dir() {
        return Ok(());
    }

    let entries = std::fs::read_dir(&overlays_dir).context("failed to read overlays directory")?;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let agent_name = entry.file_name();
        let agent_name = agent_name
            .to_str()
            .context("invalid overlay directory name")?;

        let upper_dir = entry.path().join("upper");
        if upper_dir.is_dir() {
            // Clean up stale FUSE mounts before restoring the overlay.
            cleanup_stale_fuse_mount(&entry.path(), agent_name);

            let agent_id = phantom_core::AgentId(agent_name.to_string());
            // Only register if not already tracked
            if overlays.upper_dir(&agent_id).is_err()
                && let Err(e) = overlays.create_overlay(agent_id.clone(), repo_root)
            {
                warn!(%agent_id, error = %e, "failed to restore overlay");
            }
        }
    }

    Ok(())
}

/// Check if a FUSE daemon's PID file exists and whether the process is still alive.
/// If the daemon is dead, attempt to unmount the stale FUSE mount and clean up.
fn cleanup_stale_fuse_mount(overlay_dir: &Path, agent_name: &str) {
    let pid_file = overlay_dir.join("fuse.pid");
    if !pid_file.exists() {
        return;
    }

    let pid_str = match std::fs::read_to_string(&pid_file) {
        Ok(s) => s,
        Err(_) => return,
    };

    let pid: i32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            let _ = std::fs::remove_file(&pid_file);
            return;
        }
    };

    // SAFETY: kill(pid, 0) checks if a process exists without sending a signal.
    let alive = unsafe { libc::kill(pid, 0) } == 0;

    if !alive {
        let mount_point = overlay_dir.join("mount");
        let _ = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(&mount_point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_file(&pid_file);
        warn!(agent = agent_name, pid, "cleaned up stale FUSE mount");
    }
}

/// Walk up from `start` looking for a `.phantom/` directory.
fn find_phantom_dir(start: &Path) -> anyhow::Result<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join(".phantom");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        if !current.pop() {
            bail!(
                "not a Phantom repository (no .phantom/ found above {}). Run `phantom up` first.",
                start.display()
            );
        }
    }
}
