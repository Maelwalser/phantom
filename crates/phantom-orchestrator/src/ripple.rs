//! Trunk change notification via ripple checks.
//!
//! After a changeset is materialized, [`RippleChecker`] determines which
//! active agents have in-progress work that overlaps with the changed files.
//! Those agents can then be notified to re-read affected files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use phantom_core::id::{AgentId, GitOid};
use phantom_core::notification::{TrunkFileStatus, TrunkNotification};

/// Checks which active agents are affected by trunk changes.
#[derive(Debug)]
pub struct RippleChecker;

impl RippleChecker {
    /// Create a new ripple checker.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Determine which agents are affected by a set of changed files.
    ///
    /// For each active agent, checks whether any of their touched files overlap
    /// with `changed_files`. Returns a map from agent ID to the list of
    /// overlapping file paths.
    ///
    /// All paths must be in the same canonical form (e.g. relative to repo root
    /// without `./` prefixes) for comparison to work correctly.
    #[must_use]
    pub fn check_ripple(
        changed_files: &[PathBuf],
        active_agents: &[(AgentId, Vec<PathBuf>)],
    ) -> HashMap<AgentId, Vec<PathBuf>> {
        let changed_set: HashSet<&PathBuf> = changed_files.iter().collect();
        let mut affected = HashMap::new();

        for (agent_id, agent_files) in active_agents {
            let overlapping: Vec<PathBuf> = agent_files
                .iter()
                .filter(|f| changed_set.contains(f))
                .cloned()
                .collect();

            if !overlapping.is_empty() {
                affected.insert(agent_id.clone(), overlapping);
            }
        }

        affected
    }
}

/// Classify each changed file as [`TrunkVisible`] or [`Shadowed`] for an agent.
///
/// A file is `Shadowed` if it exists in the agent's upper directory (the agent
/// still sees its old copy). Otherwise it is `TrunkVisible` (reads fall through
/// to the updated trunk).
#[must_use]
pub fn classify_trunk_changes(
    changed_files: &[PathBuf],
    upper_dir: &Path,
) -> Vec<(PathBuf, TrunkFileStatus)> {
    changed_files
        .iter()
        .map(|f| {
            let status = if upper_dir.join(f).exists() {
                TrunkFileStatus::Shadowed
            } else {
                TrunkFileStatus::TrunkVisible
            };
            (f.clone(), status)
        })
        .collect()
}

/// Write a trunk notification file for an agent.
///
/// The file is written to `.phantom/overlays/<agent>/trunk-updated.json`.
pub fn write_trunk_notification(
    phantom_dir: &Path,
    agent_id: &AgentId,
    notification: &TrunkNotification,
) -> std::io::Result<()> {
    let path = phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join("trunk-updated.json");
    let json = serde_json::to_string_pretty(notification).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Remove a stale trunk notification file if it exists.
pub fn remove_trunk_notification(phantom_dir: &Path, agent_id: &AgentId) {
    let path = phantom_dir
        .join("overlays")
        .join(&agent_id.0)
        .join("trunk-updated.json");
    let _ = std::fs::remove_file(path);
}

/// Build a [`TrunkNotification`] for an agent from classified file changes.
#[must_use]
pub fn build_notification(
    new_commit: GitOid,
    files: Vec<(PathBuf, TrunkFileStatus)>,
) -> TrunkNotification {
    TrunkNotification {
        new_commit,
        timestamp: chrono::Utc::now(),
        files,
    }
}

impl Default for RippleChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "ripple_tests.rs"]
mod tests;
