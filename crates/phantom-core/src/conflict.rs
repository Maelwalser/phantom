//! Conflict types produced during semantic merge checks.
//!
//! When two changesets modify overlapping symbols, Phantom classifies the
//! conflict and attaches enough context for the orchestrator to decide
//! whether to re-dispatch an agent or escalate to a human.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{ChangesetId, SymbolId};

/// Classification of a semantic conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConflictKind {
    /// Both changesets modified the same symbol's body.
    BothModifiedSymbol,
    /// One changeset modified a symbol that the other deleted.
    ModifyDeleteSymbol,
    /// Both changesets changed the same dependency version.
    BothModifiedDependencyVersion,
    /// Fallback: the semantic layer could not classify the conflict.
    RawTextConflict,
    /// The file is binary or not valid UTF-8; text merge would corrupt data.
    BinaryFile,
}

/// Detailed description of a single conflict between two changesets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictDetail {
    /// What kind of conflict this is.
    pub kind: ConflictKind,
    /// The file where the conflict occurs.
    pub file: PathBuf,
    /// The symbol involved, if the conflict is symbol-level.
    pub symbol_id: Option<SymbolId>,
    /// The changeset on "our" side of the merge.
    pub ours_changeset: ChangesetId,
    /// The changeset on "their" side of the merge.
    pub theirs_changeset: ChangesetId,
    /// Human-readable explanation of the conflict.
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_conflict() -> ConflictDetail {
        ConflictDetail {
            kind: ConflictKind::BothModifiedSymbol,
            file: PathBuf::from("src/handlers.rs"),
            symbol_id: Some(SymbolId("crate::handlers::login::Function".into())),
            ours_changeset: ChangesetId("cs-0040".into()),
            theirs_changeset: ChangesetId("cs-0042".into()),
            description: "Both agents modified handlers::login".into(),
        }
    }

    #[test]
    fn serde_conflict_kind_roundtrip() {
        for kind in [
            ConflictKind::BothModifiedSymbol,
            ConflictKind::ModifyDeleteSymbol,
            ConflictKind::BothModifiedDependencyVersion,
            ConflictKind::RawTextConflict,
            ConflictKind::BinaryFile,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: ConflictKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn serde_conflict_detail_roundtrip() {
        let detail = sample_conflict();
        let json = serde_json::to_string(&detail).unwrap();
        let back: ConflictDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(detail, back);
    }

    #[test]
    fn conflict_detail_without_symbol() {
        let detail = ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: PathBuf::from("Cargo.toml"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "raw text conflict in Cargo.toml".into(),
        };
        let json = serde_json::to_string(&detail).unwrap();
        let back: ConflictDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(detail, back);
    }
}
