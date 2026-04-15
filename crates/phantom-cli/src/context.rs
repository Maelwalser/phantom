//! Shared context loaded by each command.
//!
//! [`PhantomContext`] locates the `.phantom/` directory and repository root.
//! Individual subsystems (git, events, overlays, semantic) are opened lazily
//! via dedicated `open_*` methods so that commands only pay for what they use.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use phantom_events::SqliteEventStore;
use phantom_git::GitOps;
use phantom_overlay::OverlayManager;
use phantom_semantic::SemanticMerger;
use tracing::{debug, warn};

/// Lightweight handle to a Phantom-managed repository.
///
/// Holds only the paths needed to locate subsystems. Call `open_*` methods
/// to initialize individual subsystems on demand.
pub struct PhantomContext {
    pub phantom_dir: PathBuf,
    pub repo_root: PathBuf,
}

impl PhantomContext {
    /// Find `.phantom/` by walking up from the current directory.
    ///
    /// This is a cheap, synchronous operation — no subsystems are opened.
    pub fn locate() -> anyhow::Result<Self> {
        let cwd = std::env::current_dir().context("failed to determine current directory")?;
        let phantom_dir = find_phantom_dir(&cwd)?;
        let repo_root = phantom_dir
            .parent()
            .expect(".phantom/ must have a parent")
            .to_path_buf();

        Ok(Self {
            phantom_dir,
            repo_root,
        })
    }

    /// Open the git repository handle.
    pub fn open_git(&self) -> anyhow::Result<GitOps> {
        GitOps::open(&self.repo_root).context("failed to open git repository")
    }

    /// Open the SQLite event store.
    pub async fn open_events(&self) -> anyhow::Result<SqliteEventStore> {
        let events_path = self.phantom_dir.join("events.db");
        SqliteEventStore::open(&events_path)
            .await
            .context("failed to open event store")
    }

    /// Create an overlay manager (without restoring existing overlays).
    pub fn open_overlays(&self) -> OverlayManager {
        OverlayManager::new(self.phantom_dir.clone())
    }

    /// Create an overlay manager and restore existing overlays from disk.
    ///
    /// This scans `.phantom/overlays/` for agent directories and re-registers
    /// them, cleaning up stale FUSE mounts and dead agent processes. Only call
    /// from commands that interact with overlays.
    pub fn open_overlays_restored(&self) -> anyhow::Result<OverlayManager> {
        let mut mgr = self.open_overlays();
        restore_overlays(&mut mgr, &self.phantom_dir, &self.repo_root)?;
        Ok(mgr)
    }

    /// Create a new semantic merger.
    pub fn semantic(&self) -> SemanticMerger {
        SemanticMerger::new()
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
            // Clean up stale FUSE mounts and agent processes before restoring.
            cleanup_stale_fuse_mount(&entry.path(), agent_name);
            cleanup_stale_agent_process(&entry.path(), agent_name);

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

    let record = match crate::pid_guard::read_pid_file(&pid_file) {
        Some(r) => r,
        None => {
            let _ = std::fs::remove_file(&pid_file);
            return;
        }
    };

    if !crate::pid_guard::is_process_alive(&record) {
        let mount_point = overlay_dir.join("mount");
        let _ = std::process::Command::new("fusermount3")
            .arg("-u")
            .arg(&mount_point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_file(&pid_file);
        debug!(agent = agent_name, pid = record.pid, "cleaned up stale FUSE mount");
    }
}

/// Check if a background agent's PID file exists and whether the process is still alive.
/// If the process is dead and no `agent.status` was written, write a failed status marker.
fn cleanup_stale_agent_process(overlay_dir: &Path, agent_name: &str) {
    let pid_file = overlay_dir.join("agent.pid");
    if !pid_file.exists() {
        return;
    }

    let record = match crate::pid_guard::read_pid_file(&pid_file) {
        Some(r) => r,
        None => {
            let _ = std::fs::remove_file(&pid_file);
            return;
        }
    };

    if !crate::pid_guard::is_process_alive(&record) {
        let status_file = overlay_dir.join("agent.status");
        if !status_file.exists() {
            // Process died without writing a status file — write a failed marker.
            let status = crate::commands::agent_monitor::AgentStatus {
                exit_code: None,
                completed_at: chrono::Utc::now(),
                materialized: false,
                error: Some("agent process died unexpectedly (no status written)".into()),
            };
            if let Ok(json) = serde_json::to_string_pretty(&status) {
                let _ = std::fs::write(&status_file, json);
            }
            warn!(
                agent = agent_name,
                pid = record.pid, "detected dead agent process without status"
            );
        }
        // Clean up PID files.
        let _ = std::fs::remove_file(&pid_file);
        let monitor_pid = overlay_dir.join("monitor.pid");
        let _ = std::fs::remove_file(&monitor_pid);
    }
}

/// Read `default_cli` from `.phantom/config.toml`.
///
/// Falls back to `"claude"` if the key is missing or the config is unreadable.
/// Uses simple line parsing to avoid pulling in a TOML crate.
pub fn default_cli(phantom_dir: &Path) -> String {
    let config_path = phantom_dir.join("config.toml");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return "claude".to_string(),
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("default_cli") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let val = rest.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return val.to_string();
                }
            }
        }
    }

    "claude".to_string()
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
                "not a Phantom repository (no .phantom/ found above {}). Run `phantom init` first.",
                start.display()
            );
        }
    }
}
