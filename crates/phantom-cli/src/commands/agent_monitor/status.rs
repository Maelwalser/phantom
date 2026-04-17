//! Completion status written to `.phantom/overlays/<agent>/agent.status`.

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Completion status written to `.phantom/overlays/<agent>/agent.status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStatus {
    /// Exit code of the claude process (None if killed by signal).
    pub exit_code: Option<i32>,
    /// When the agent process completed.
    pub completed_at: chrono::DateTime<Utc>,
    /// Whether the changeset was successfully materialized.
    pub materialized: bool,
    /// Error message if something went wrong during post-completion.
    pub error: Option<String>,
}
