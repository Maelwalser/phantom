//! Shared context loaded by each command.
//!
//! [`PhantomContext`] locates the `.phantom/` directory and repository root.
//! Individual subsystems (git, events, overlays, semantic) are opened lazily
//! via dedicated `open_*` methods so that commands only pay for what they use.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use phantom_core::GitOid;
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
            .ok_or_else(|| {
                anyhow::anyhow!(
                    ".phantom/ resolved to the filesystem root ({}); this is unsupported",
                    phantom_dir.display()
                )
            })?
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
    #[allow(clippy::unused_self)]
    pub fn semantic(&self) -> SemanticMerger {
        SemanticMerger::new()
    }
}

/// Reject operations that require a real commit to anchor against.
///
/// Materialization reads `parent_oid` via `find_commit()`; the null OID
/// (returned by `GitOps::head_oid()` when HEAD is unborn) causes libgit2 to
/// bail with `odb: cannot read object: null OID cannot exist`. We fail early
/// with a clear action instead of letting the agent do work that can never be
/// committed.
pub fn require_initialized_head(head: &GitOid) -> anyhow::Result<()> {
    if *head == GitOid::zero() {
        bail!(
            "repository has no initial commit. Run `ph init` to auto-create one, or `git commit --allow-empty -m \"initial\"` before dispatching agents."
        );
    }
    Ok(())
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
            // Skip directories whose name is not a valid agent ID. This
            // prevents a manually-created `../escape/upper/` (or a directory
            // name containing control chars) from flowing into path joins and
            // log lines. Legitimate overlays created via `ph <agent>` always
            // pass this check.
            let agent_id = match phantom_core::AgentId::validate(agent_name) {
                Ok(id) => id,
                Err(e) => {
                    warn!(
                        dir_name = %agent_name,
                        error = %e,
                        "skipping overlay directory with invalid agent name"
                    );
                    continue;
                }
            };

            // Clean up stale FUSE mounts and agent processes before restoring.
            cleanup_stale_fuse_mount(&entry.path(), agent_name);
            cleanup_stale_agent_process(&entry.path(), agent_name);

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

    let Some(record) = crate::pid_guard::read_pid_file(&pid_file) else {
        let _ = std::fs::remove_file(&pid_file);
        return;
    };

    if !crate::pid_guard::is_process_alive(&record) {
        let mount_point = overlay_dir.join("mount");
        let _ = crate::fs::fuse::unmount(&mount_point);
        let _ = std::fs::remove_file(&pid_file);
        debug!(
            agent = agent_name,
            pid = record.pid,
            "cleaned up stale FUSE mount"
        );
    }
}

/// Check if a background agent's PID file exists and whether the process is still alive.
/// If the process is dead and no `agent.status` was written, write a failed status marker.
fn cleanup_stale_agent_process(overlay_dir: &Path, agent_name: &str) {
    let pid_file = overlay_dir.join("agent.pid");
    if !pid_file.exists() {
        return;
    }

    let Some(record) = crate::pid_guard::read_pid_file(&pid_file) else {
        let _ = std::fs::remove_file(&pid_file);
        return;
    };

    if !crate::pid_guard::is_process_alive(&record) {
        // The CLI child exited. Don't declare failure while the monitor is
        // still orchestrating the post-session flow (submit + materialize),
        // which runs for tens of seconds on large first commits. Only the
        // monitor writes `agent.status`; racing it here would plant a
        // "died unexpectedly" marker on a healthy submit.
        let monitor_pid_file = overlay_dir.join("monitor.pid");
        let monitor_alive = crate::pid_guard::read_pid_file(&monitor_pid_file)
            .is_some_and(|r| crate::pid_guard::is_process_alive(&r));
        if monitor_alive {
            return;
        }

        let status_file = overlay_dir.join("agent.status");
        if !status_file.exists() {
            // Both CLI and monitor are gone — real crash, record it.
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
                pid = record.pid,
                "detected dead agent process without status"
            );
        }
        // Clean up PID files.
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(&monitor_pid_file);
    }
}

/// CLI binaries Phantom knows how to launch. Any other `default_cli` value
/// is rejected to prevent `.phantom/config.toml` from becoming an arbitrary
/// command execution sink: the parsed value flows directly into
/// `Command::new(...)`, so an attacker-controlled config (via a merged PR
/// or rogue clone) could otherwise launch any binary on the host.
pub const KNOWN_CLIS: &[&str] = &["claude", "gemini", "opencode"];

/// Validate a CLI name is on the allowlist and has no path separators.
///
/// Rejects anything containing `/`, `\`, null bytes, or unknown basenames.
/// Returns a descriptive error so the operator can fix the config.
pub fn validate_cli_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("CLI name must not be empty".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(format!(
            "CLI name '{name}' contains a path separator or null byte — only bare CLI names are allowed"
        ));
    }
    if !KNOWN_CLIS.contains(&name) {
        // Escape hatch for the integration test harness: when the CLI is
        // stubbed with `echo` (or similar), the test process sets
        // PHANTOM_ALLOW_ANY_CLI=1 to bypass the allowlist. This is a test-
        // only flag and is NOT documented as a user-facing feature.
        let allow_any = std::env::var("PHANTOM_ALLOW_ANY_CLI")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        if !allow_any {
            return Err(format!(
                "CLI name '{name}' is not in the allowlist {KNOWN_CLIS:?}; \
                 add support via phantom-session::adapter before use"
            ));
        }
    }
    Ok(())
}

/// Read `default_cli` from `.phantom/config.toml`.
///
/// Falls back to `"claude"` if the key is missing, the config is unreadable,
/// or the configured value fails the allowlist check. Logs a warning on the
/// rejected-value path so operators notice attempted injections.
pub fn default_cli(phantom_dir: &Path) -> String {
    let config_path = phantom_dir.join("config.toml");
    let Ok(content) = std::fs::read_to_string(&config_path) else {
        return "claude".to_string();
    };

    for line in content.lines() {
        // Strip whitespace and inline comments (`# ...`).
        let mut trimmed = line.trim();
        if let Some(idx) = trimmed.find('#') {
            trimmed = trimmed[..idx].trim();
        }
        if let Some(rest) = trimmed.strip_prefix("default_cli") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let val = rest.trim().trim_matches('"').trim_matches('\'');
                if val.is_empty() {
                    continue;
                }
                match validate_cli_name(val) {
                    Ok(()) => return val.to_string(),
                    Err(e) => {
                        tracing::warn!(
                            config = %config_path.display(),
                            value = %val,
                            error = %e,
                            "rejecting default_cli; falling back to 'claude'"
                        );
                        return "claude".to_string();
                    }
                }
            }
        }
    }

    "claude".to_string()
}

/// Walk up from `start` looking for a `.phantom/` directory.
///
/// Uses `symlink_metadata` rather than `is_dir` so that a `.phantom/` entry
/// that is a symlink to somewhere outside the repository is rejected —
/// otherwise every subsequent read/write (event DB, overlay paths, PID
/// files) would silently follow the symlink target.
fn find_phantom_dir(start: &Path) -> anyhow::Result<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join(".phantom");
        if let Ok(meta) = std::fs::symlink_metadata(&candidate) {
            if meta.file_type().is_dir() {
                return Ok(candidate);
            }
            if meta.file_type().is_symlink() {
                bail!(
                    ".phantom/ at {} is a symlink; refusing to use it. \
                     Replace the symlink with a real directory or run `ph init` \
                     at the intended location.",
                    candidate.display()
                );
            }
        }
        if !current.pop() {
            bail!(
                "not a Phantom repository (no .phantom/ found above {}). Run `ph init` first.",
                start.display()
            );
        }
    }
}
