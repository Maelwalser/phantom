//! Trunk change notification types.
//!
//! When a changeset is materialized, each affected agent receives a
//! [`TrunkNotification`] describing which files changed, whether the agent's
//! upper layer shadows the new trunk version, and which of the agent's
//! working-set symbols depend on something that just changed.

use std::ops::Range;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{GitOid, SymbolId};
use crate::symbol::ReferenceKind;

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

/// How a referenced trunk symbol changed — used to rank impact severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImpactChange {
    /// The symbol's declaration (signature) changed. Likely API-breaking for
    /// dependents — ranked highest-severity.
    SignatureChanged,
    /// The symbol's body changed but its declaration is stable. Usually safe
    /// for dependents.
    BodyOnlyChanged,
    /// The symbol was removed from trunk. Dependents must update.
    Deleted,
    /// A new symbol was added whose shape may collide with a dependent's
    /// expectations (e.g. shadowing). Emitted only when there is a pre-existing
    /// reference by name to this newly-added symbol.
    Added,
}

impl ImpactChange {
    /// Numeric severity for deterministic sorting — higher is worse.
    #[must_use]
    pub fn severity(self) -> u8 {
        match self {
            Self::Deleted => 3,
            Self::SignatureChanged => 2,
            Self::Added => 1,
            Self::BodyOnlyChanged => 0,
        }
    }

    /// Short human label used in notification markdown (e.g. "signature changed").
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SignatureChanged => "signature changed",
            Self::BodyOnlyChanged => "body changed",
            Self::Deleted => "deleted",
            Self::Added => "added (name collision)",
        }
    }
}

/// A single impact a trunk change has on one of the agent's working-set symbols.
///
/// Produced by the orchestrator after materialization: for each changed
/// trunk symbol, the orchestrator queries the dependency graph's reverse
/// edges, then intersects with the agent's upper-layer symbols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyImpact {
    /// A symbol in the agent's upper layer that holds a reference.
    pub your_symbol: SymbolId,
    /// The trunk symbol that changed.
    pub depends_on: SymbolId,
    /// Kind of change that hit `depends_on`.
    pub change: ImpactChange,
    /// How the reference is expressed (call / type-use / import / ...).
    pub edge_kind: ReferenceKind,
    /// File in the agent's working copy where the reference lives.
    pub file: PathBuf,
    /// Byte range of the reference node (inclusive start, exclusive end).
    pub byte_range: Range<usize>,
    /// 1-based line numbers (start, end) of the reference, for display.
    pub line_range: (u32, u32),
    /// Optional short preview of the new trunk symbol (first N chars of its
    /// declaration). `None` if no preview could be produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trunk_preview: Option<String>,
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
    /// Per-symbol dependency impacts for the agent's working set.
    ///
    /// Tagged with `#[serde(default)]` so that payloads written by older
    /// Phantom binaries (before the dependency graph existed) deserialize
    /// cleanly with an empty vector.
    #[serde(default)]
    pub dependency_impacts: Vec<DependencyImpact>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SymbolId;

    #[test]
    fn serde_trunk_notification_roundtrip() {
        let n = TrunkNotification {
            new_commit: GitOid::zero(),
            timestamp: Utc::now(),
            files: vec![(PathBuf::from("src/lib.rs"), TrunkFileStatus::Shadowed)],
            dependency_impacts: vec![DependencyImpact {
                your_symbol: SymbolId("crate::user::Function".into()),
                depends_on: SymbolId("crate::api::Function".into()),
                change: ImpactChange::SignatureChanged,
                edge_kind: ReferenceKind::Call,
                file: PathBuf::from("src/user.rs"),
                byte_range: 100..110,
                line_range: (7, 7),
                trunk_preview: Some("fn api(new_arg: u32)".into()),
            }],
        };
        let json = serde_json::to_string(&n).unwrap();
        let back: TrunkNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn trunk_notification_decodes_without_dependency_impacts() {
        // Older notifications written before the dependency graph must
        // still deserialize — the missing field defaults to an empty vec.
        let legacy = r#"{
            "new_commit": [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "timestamp": "2026-04-21T10:00:00Z",
            "files": []
        }"#;
        let n: TrunkNotification = serde_json::from_str(legacy).unwrap();
        assert!(n.dependency_impacts.is_empty());
    }

    #[test]
    fn impact_change_severity_ordering() {
        assert!(ImpactChange::Deleted.severity() > ImpactChange::SignatureChanged.severity());
        assert!(ImpactChange::SignatureChanged.severity() > ImpactChange::Added.severity());
        assert!(ImpactChange::Added.severity() > ImpactChange::BodyOnlyChanged.severity());
    }
}
