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
    #[allow(clippy::naive_bytecount)]
    pub fn from_byte_range(src: &[u8], byte_range: Range<usize>) -> Self {
        let start_byte = byte_range.start.min(src.len());
        let start_line = src[..start_byte].iter().filter(|&&b| b == b'\n').count() + 1;
        let end_byte = byte_range.end.min(src.len());
        let end_line = src[..end_byte].iter().filter(|&&b| b == b'\n').count() + 1;
        Self {
            byte_range,
            start_line,
            end_line,
        }
    }
}

/// Result of a semantic merge check between a changeset and trunk.
///
/// Emitted by the merge-check step of the submit pipeline and embedded
/// in [`EventKind::ChangesetMergeChecked`](crate::event::EventKind::ChangesetMergeChecked).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeCheckResult {
    /// The changeset merges cleanly with trunk.
    Clean,
    /// The changeset has symbol-level conflicts.
    Conflicted(Vec<ConflictDetail>),
}

/// Outcome of a three-way semantic merge.
///
/// Returned by [`SemanticAnalyzer::three_way_merge`](crate::traits::SemanticAnalyzer::three_way_merge)
/// as part of a [`MergeReport`] carrying the strategy used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeResult {
    /// The merge produced clean output.
    Clean(Vec<u8>),
    /// The merge found conflicts that require re-tasking.
    Conflict(Vec<ConflictDetail>),
}

/// Which algorithm produced a [`MergeResult`].
///
/// The semantic merger prefers symbol-level analysis but falls back to a
/// line-based text merge in several situations (unsupported language,
/// semantic-merger error, syntax errors in the reconstructed output).
/// Callers surface this to users so they know when a merge was decided by
/// plain text diff rather than syntactic understanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MergeStrategy {
    /// Full tree-sitter-based semantic merge succeeded end-to-end.
    Semantic,
    /// Short-circuit: one side equals base, or both sides are identical.
    /// Conceptually accurate; no parse required.
    Trivial,
    /// Language has no tree-sitter extractor — used line-based merge only.
    TextFallbackUnsupported,
    /// Semantic merger returned an error (parse failure, malformed input).
    TextFallbackSemanticError,
    /// Semantic merge produced output that failed to re-parse cleanly.
    TextFallbackInvalidSyntax,
}

impl MergeStrategy {
    /// Returns `true` if this strategy represents a fallback to line-based
    /// text merge.  Useful for gating user-facing warnings.
    #[must_use]
    pub fn is_text_fallback(&self) -> bool {
        matches!(
            self,
            Self::TextFallbackUnsupported
                | Self::TextFallbackSemanticError
                | Self::TextFallbackInvalidSyntax,
        )
    }

    /// Human-readable short name of the strategy, suitable for CLI output.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Trivial => "trivial",
            Self::TextFallbackUnsupported => "text (unsupported language)",
            Self::TextFallbackSemanticError => "text (semantic merger error)",
            Self::TextFallbackInvalidSyntax => "text (invalid merged syntax)",
        }
    }
}

/// A merge outcome plus the strategy that produced it.
///
/// Additive wrapper around [`MergeResult`] introduced so callers can tell
/// a semantic merge apart from a text-level fallback without parsing log
/// lines.  Destructure `report.result` if you only care about clean / conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReport {
    /// The actual merge outcome (clean bytes or conflict list).
    pub result: MergeResult,
    /// Which algorithm produced `result`.
    pub strategy: MergeStrategy,
}

impl MergeReport {
    /// Construct a report flagged as produced by full semantic analysis.
    #[must_use]
    pub fn semantic(result: MergeResult) -> Self {
        Self {
            result,
            strategy: MergeStrategy::Semantic,
        }
    }

    /// Construct a trivially-decided report (short-circuit path).
    #[must_use]
    pub fn trivial(result: MergeResult) -> Self {
        Self {
            result,
            strategy: MergeStrategy::Trivial,
        }
    }

    /// Construct a report produced by a text-level fallback.
    #[must_use]
    pub fn text_fallback(result: MergeResult, reason: MergeStrategy) -> Self {
        debug_assert!(
            reason.is_text_fallback(),
            "text_fallback requires a fallback strategy"
        );
        Self {
            result,
            strategy: reason,
        }
    }
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
            ours_span: Some(ConflictSpan {
                byte_range: 100..200,
                start_line: 5,
                end_line: 10,
            }),
            theirs_span: Some(ConflictSpan {
                byte_range: 100..250,
                start_line: 5,
                end_line: 12,
            }),
            base_span: Some(ConflictSpan {
                byte_range: 100..180,
                start_line: 5,
                end_line: 9,
            }),
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
    fn span_from_byte_range_computes_lines() {
        let src = b"line1\nline2\nline3\nline4\n";
        //          0----5 6----11 12---17 18---23

        // Byte 6 is start of line 2, byte 17 is end of line 3
        let span = ConflictSpan::from_byte_range(src, 6..17);
        assert_eq!(span.start_line, 2);
        assert_eq!(span.end_line, 3);
        assert_eq!(span.byte_range, 6..17);
    }

    #[test]
    fn span_from_byte_range_first_line() {
        let src = b"fn main() {}";
        let span = ConflictSpan::from_byte_range(src, 0..12);
        assert_eq!(span.start_line, 1);
        assert_eq!(span.end_line, 1);
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
            ours_span: None,
            theirs_span: None,
            base_span: None,
        };
        let json = serde_json::to_string(&detail).unwrap();
        let back: ConflictDetail = serde_json::from_str(&json).unwrap();
        assert_eq!(detail, back);
    }

    #[test]
    fn serde_merge_check_result_roundtrip() {
        let clean = MergeCheckResult::Clean;
        let json = serde_json::to_string(&clean).unwrap();
        let back: MergeCheckResult = serde_json::from_str(&json).unwrap();
        assert_eq!(clean, back);

        let conflicted = MergeCheckResult::Conflicted(vec![ConflictDetail {
            kind: ConflictKind::BothModifiedSymbol,
            file: PathBuf::from("src/lib.rs"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "test conflict".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }]);
        let json = serde_json::to_string(&conflicted).unwrap();
        let back: MergeCheckResult = serde_json::from_str(&json).unwrap();
        assert_eq!(conflicted, back);
    }

    #[test]
    fn serde_merge_result_roundtrip() {
        let clean = MergeResult::Clean(b"merged output".to_vec());
        let json = serde_json::to_string(&clean).unwrap();
        let back: MergeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(clean, back);

        let conflict = MergeResult::Conflict(vec![]);
        let json = serde_json::to_string(&conflict).unwrap();
        let back: MergeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(conflict, back);
    }

    #[test]
    fn merge_strategy_classifies_fallbacks() {
        assert!(!MergeStrategy::Semantic.is_text_fallback());
        assert!(!MergeStrategy::Trivial.is_text_fallback());
        assert!(MergeStrategy::TextFallbackUnsupported.is_text_fallback());
        assert!(MergeStrategy::TextFallbackSemanticError.is_text_fallback());
        assert!(MergeStrategy::TextFallbackInvalidSyntax.is_text_fallback());
    }

    #[test]
    fn serde_merge_strategy_roundtrip() {
        for s in [
            MergeStrategy::Semantic,
            MergeStrategy::Trivial,
            MergeStrategy::TextFallbackUnsupported,
            MergeStrategy::TextFallbackSemanticError,
            MergeStrategy::TextFallbackInvalidSyntax,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: MergeStrategy = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn serde_merge_report_roundtrip() {
        let report = MergeReport::semantic(MergeResult::Clean(b"ok".to_vec()));
        let json = serde_json::to_string(&report).unwrap();
        let back: MergeReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);

        let fallback = MergeReport::text_fallback(
            MergeResult::Clean(b"textual".to_vec()),
            MergeStrategy::TextFallbackInvalidSyntax,
        );
        let json = serde_json::to_string(&fallback).unwrap();
        let back: MergeReport = serde_json::from_str(&json).unwrap();
        assert_eq!(fallback, back);
    }
}
