//! Conflict types produced during semantic merge checks.
//!
//! When two changesets modify overlapping symbols, Phantom classifies the
//! conflict and attaches enough context for the orchestrator to decide
//! whether to re-task an agent or escalate to a human.

use std::ops::Range;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{ChangesetId, SymbolId};

/// Byte-level location of one side of a conflict within a file.
///
/// Captures enough positional context so downstream consumers (CLI,
/// orchestrator, agent wrappers) can render a conflict visualization
/// without re-parsing the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictSpan {
    /// Byte range of the conflicting region within the file.
    pub byte_range: Range<usize>,
    /// One-indexed start line (computed from source bytes for display).
    pub start_line: usize,
    /// One-indexed end line (inclusive).
    pub end_line: usize,
}

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
    /// Location of the conflict in the "ours" version of the file, if known.
    pub ours_span: Option<ConflictSpan>,
    /// Location of the conflict in the "theirs" version of the file, if known.
    pub theirs_span: Option<ConflictSpan>,
    /// Location of the symbol in the base version, if known.
    pub base_span: Option<ConflictSpan>,
}

impl ConflictSpan {
    /// Build a [`ConflictSpan`] from source bytes and a byte range.
    ///
    /// Computes one-indexed line numbers by counting newlines in `src`
    /// up to the range boundaries.
    pub fn from_byte_range(src: &[u8], byte_range: Range<usize>) -> Self {
        let start_line = src[..byte_range.start]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1;
        let end_byte = byte_range.end.min(src.len());
        let end_line = src[..end_byte].iter().filter(|&&b| b == b'\n').count() + 1;
        Self {
            byte_range,
            start_line,
            end_line,
        }
    }
}

#[cfg(test)]
#[path = "conflict_tests.rs"]
mod tests;
