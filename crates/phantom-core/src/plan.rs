//! Plan types for multi-domain task decomposition.
//!
//! A [`Plan`] represents a high-level feature request decomposed into
//! independent [`PlanDomain`]s, each executed by a separate background agent.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::PlanId;

/// A decomposed implementation plan ready for dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// Unique plan identifier (e.g. `"plan-20260413-143022"`).
    pub id: PlanId,
    /// The original user request.
    pub request: String,
    /// When the plan was created.
    pub created_at: DateTime<Utc>,
    /// Independent domains to execute in parallel.
    pub domains: Vec<PlanDomain>,
    /// Current plan status.
    pub status: PlanStatus,
}

/// A single domain within a plan — an independent unit of work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDomain {
    /// Short kebab-case name (e.g. `"rate-limiting"`).
    pub name: String,
    /// Agent identifier (e.g. `"plan-20260413-rate-limiting"`).
    pub agent_id: String,
    /// What this domain implements.
    pub description: String,
    /// Files the agent should modify.
    pub files_to_modify: Vec<PathBuf>,
    /// Files the agent must not touch (owned by other domains).
    pub files_not_to_modify: Vec<String>,
    /// Requirements checklist.
    pub requirements: Vec<String>,
    /// Verification commands to run before finishing.
    pub verification: Vec<String>,
    /// Names of other domains this one depends on.
    pub depends_on: Vec<String>,
}

/// Lifecycle status of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanStatus {
    /// Plan generated, awaiting user confirmation.
    Draft,
    /// User confirmed, agents not yet dispatched.
    Confirmed,
    /// Agents dispatched and running.
    Dispatched,
    /// All agents completed successfully.
    Completed,
    /// Some agents failed.
    PartiallyFailed,
}

/// Raw domain data parsed from the AI planner's JSON output.
///
/// This is the deserialization target before we assign agent IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPlanDomain {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub files_to_modify: Vec<PathBuf>,
    #[serde(default)]
    pub files_not_to_modify: Vec<String>,
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Raw planner output before conversion to a full [`Plan`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPlanOutput {
    pub domains: Vec<RawPlanDomain>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_plan_output_deserializes() {
        let json = r#"{
            "domains": [
                {
                    "name": "rate-limiting",
                    "description": "Add rate limiting middleware",
                    "files_to_modify": ["src/middleware.rs"],
                    "requirements": ["Token bucket algorithm"],
                    "verification": ["cargo test"]
                }
            ]
        }"#;
        let output: RawPlanOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.domains.len(), 1);
        assert_eq!(output.domains[0].name, "rate-limiting");
    }

    #[test]
    fn plan_serde_roundtrip() {
        let plan = Plan {
            id: PlanId("plan-20260413-143022".into()),
            request: "add caching".into(),
            created_at: Utc::now(),
            domains: vec![PlanDomain {
                name: "cache".into(),
                agent_id: "plan-20260413-cache".into(),
                description: "Add cache layer".into(),
                files_to_modify: vec!["src/cache.rs".into()],
                files_not_to_modify: vec![],
                requirements: vec!["LRU cache".into()],
                verification: vec!["cargo test".into()],
                depends_on: vec![],
            }],
            status: PlanStatus::Draft,
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, plan.id);
        assert_eq!(back.domains.len(), 1);
    }
}
