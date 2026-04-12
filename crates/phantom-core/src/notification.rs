//! Trunk change notification types.
//!
//! When a changeset is materialized, each affected agent receives a
//! [`TrunkNotification`] describing which files changed and whether
//! the agent's upper layer shadows the new trunk version.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::GitOid;

/// How a trunk-changed file appears to an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrunkFileStatus {
    /// File is not in the agent's upper layer — the agent sees the new trunk
    /// version automatically.
    TrunkVisible,
    /// File exists in the agent's upper layer — the agent still sees its old
    /// copy and the new trunk version is hidden.
    Shadowed,
    /// Shadowed file was cleanly merged into the agent's upper layer via live
    /// rebase. The agent now sees the merged version.
    RebaseMerged,
    /// Shadowed file had merge conflicts during live rebase. The agent's upper
    /// copy was left unchanged.
    RebaseConflict,
}

/// Notification that trunk changed files relevant to an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrunkNotification {
    /// The new trunk HEAD after materialization.
    pub new_commit: GitOid,
    /// When the notification was created.
    pub timestamp: DateTime<Utc>,
    /// Changed files with their visibility status for this agent.
    pub files: Vec<(PathBuf, TrunkFileStatus)>,
}
