//! Builder helpers for constructing test fixtures.

#![allow(dead_code)]

use std::ops::Range;
use std::path::PathBuf;

use chrono::Utc;
use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::id::{AgentId, ChangesetId, ContentHash, GitOid, SymbolId};
use phantom_core::notification::{DependencyImpact, ImpactChange};
use phantom_core::symbol::{ReferenceKind, SymbolReference};
use phantom_core::traits::DependencyEdge;

/// Build a minimal [`Changeset`] for unit/integration tests.
pub fn make_changeset(id: &str, base: GitOid, files: Vec<PathBuf>) -> Changeset {
    Changeset {
        id: ChangesetId(id.into()),
        agent_id: AgentId("agent-test".into()),
        task: "test task".into(),
        base_commit: base,
        files_touched: files,
        operations: vec![],
        test_result: None,
        created_at: Utc::now(),
        status: ChangesetStatus::Submitted,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
}

/// Build a [`SymbolReference`] with sensible defaults for tests.
pub fn make_reference(
    source: &str,
    target_name: &str,
    kind: ReferenceKind,
    file: &str,
) -> SymbolReference {
    SymbolReference {
        source: SymbolId(source.into()),
        target_name: target_name.into(),
        target_scope_hint: None,
        kind,
        file: PathBuf::from(file),
        byte_range: 0..target_name.len(),
    }
}

/// Build a [`SymbolReference`] with an explicit scope hint.
pub fn make_scoped_reference(
    source: &str,
    target_name: &str,
    scope_hint: &str,
    kind: ReferenceKind,
    file: &str,
    byte_range: Range<usize>,
) -> SymbolReference {
    SymbolReference {
        source: SymbolId(source.into()),
        target_name: target_name.into(),
        target_scope_hint: Some(scope_hint.into()),
        kind,
        file: PathBuf::from(file),
        byte_range,
    }
}

/// Build a [`DependencyEdge`] for unit tests of the graph and impact modules.
pub fn make_edge(source: &str, target: &str, kind: ReferenceKind, file: &str) -> DependencyEdge {
    DependencyEdge {
        source: SymbolId(source.into()),
        target: SymbolId(target.into()),
        kind,
        file: PathBuf::from(file),
        byte_range: 0..10,
    }
}

/// Build a [`DependencyImpact`] with defaults suitable for most tests.
pub fn make_impact(
    your_symbol: &str,
    depends_on: &str,
    change: ImpactChange,
    kind: ReferenceKind,
    file: &str,
) -> DependencyImpact {
    DependencyImpact {
        your_symbol: SymbolId(your_symbol.into()),
        depends_on: SymbolId(depends_on.into()),
        change,
        edge_kind: kind,
        file: PathBuf::from(file),
        byte_range: 0..10,
        line_range: (1, 1),
        trunk_preview: None,
    }
}

/// Alias for `ContentHash::zero()` to reduce noise in test struct literals.
pub fn zero_hash() -> ContentHash {
    ContentHash::zero()
}
