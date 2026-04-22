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

/// Classify each changed file as [`TrunkFileStatus::TrunkVisible`] or
/// [`TrunkFileStatus::Shadowed`] for an agent.
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
///
/// `dependency_impacts` is passed through directly — callers computed it at
/// ripple time against the semantic dependency graph.
#[must_use]
pub fn build_notification(
    new_commit: GitOid,
    files: Vec<(PathBuf, TrunkFileStatus)>,
    dependency_impacts: Vec<phantom_core::notification::DependencyImpact>,
) -> TrunkNotification {
    TrunkNotification {
        new_commit,
        timestamp: chrono::Utc::now(),
        files,
        dependency_impacts,
    }
}

impl Default for RippleChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, files: &[&str]) -> (AgentId, Vec<PathBuf>) {
        (
            AgentId(name.into()),
            files.iter().map(PathBuf::from).collect(),
        )
    }

    #[test]
    fn overlap_affects_only_matching_agent() {
        let changed = vec![PathBuf::from("src/db.rs")];
        let agents = vec![
            agent("agent-a", &["src/api.rs"]),
            agent("agent-b", &["src/db.rs", "src/cache.rs"]),
        ];

        let result = RippleChecker::check_ripple(&changed, &agents);

        assert!(!result.contains_key(&AgentId("agent-a".into())));
        assert_eq!(
            result.get(&AgentId("agent-b".into())).unwrap(),
            &vec![PathBuf::from("src/db.rs")]
        );
    }

    #[test]
    fn no_overlap_returns_empty() {
        let changed = vec![PathBuf::from("src/unrelated.rs")];
        let agents = vec![
            agent("agent-a", &["src/api.rs"]),
            agent("agent-b", &["src/db.rs"]),
        ];

        let result = RippleChecker::check_ripple(&changed, &agents);
        assert!(result.is_empty());
    }

    #[test]
    fn multiple_overlapping_files() {
        let changed = vec![PathBuf::from("src/db.rs"), PathBuf::from("src/cache.rs")];
        let agents = vec![agent(
            "agent-a",
            &["src/db.rs", "src/cache.rs", "src/api.rs"],
        )];

        let result = RippleChecker::check_ripple(&changed, &agents);
        let affected = result.get(&AgentId("agent-a".into())).unwrap();
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&PathBuf::from("src/db.rs")));
        assert!(affected.contains(&PathBuf::from("src/cache.rs")));
    }

    #[test]
    fn same_file_touched_by_multiple_agents() {
        let changed = vec![PathBuf::from("src/shared.rs")];
        let agents = vec![
            agent("agent-a", &["src/shared.rs"]),
            agent("agent-b", &["src/shared.rs", "src/other.rs"]),
        ];

        let result = RippleChecker::check_ripple(&changed, &agents);
        assert_eq!(result.len(), 2);
        assert!(result.contains_key(&AgentId("agent-a".into())));
        assert!(result.contains_key(&AgentId("agent-b".into())));
    }

    #[test]
    fn no_agents_returns_empty() {
        let changed = vec![PathBuf::from("src/main.rs")];
        let result = RippleChecker::check_ripple(&changed, &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn no_changed_files_returns_empty() {
        let agents = vec![agent("agent-a", &["src/api.rs"])];
        let result = RippleChecker::check_ripple(&[], &agents);
        assert!(result.is_empty());
    }

    #[test]
    fn classify_shadowed_when_file_in_upper() {
        let tmp = tempfile::tempdir().unwrap();
        let upper = tmp.path();
        // Create a file in the upper directory to simulate agent modification.
        std::fs::create_dir_all(upper.join("src")).unwrap();
        std::fs::write(upper.join("src/db.rs"), "modified").unwrap();

        let changed = vec![PathBuf::from("src/db.rs"), PathBuf::from("src/api.rs")];
        let classified = classify_trunk_changes(&changed, upper);

        assert_eq!(classified.len(), 2);
        assert_eq!(
            classified[0],
            (PathBuf::from("src/db.rs"), TrunkFileStatus::Shadowed)
        );
        assert_eq!(
            classified[1],
            (PathBuf::from("src/api.rs"), TrunkFileStatus::TrunkVisible)
        );
    }

    #[test]
    fn classify_all_visible_when_upper_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let changed = vec![PathBuf::from("src/main.rs")];
        let classified = classify_trunk_changes(&changed, tmp.path());

        assert_eq!(classified.len(), 1);
        assert_eq!(classified[0].1, TrunkFileStatus::TrunkVisible);
    }
}
