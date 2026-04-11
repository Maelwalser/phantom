//! Trunk change notification via ripple checks.
//!
//! After a changeset is materialized, [`RippleChecker`] determines which
//! active agents have in-progress work that overlaps with the changed files.
//! Those agents can then be notified to re-read affected files.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use phantom_core::id::AgentId;

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
        let changed = vec![
            PathBuf::from("src/db.rs"),
            PathBuf::from("src/cache.rs"),
        ];
        let agents = vec![agent("agent-a", &["src/db.rs", "src/cache.rs", "src/api.rs"])];

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
}
