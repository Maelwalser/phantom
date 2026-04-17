//! Integration test: the semantic merger reports a text-fallback strategy
//! when given a file whose language has no tree-sitter extractor.
//!
//! Pins the `MergeReport.strategy` surface added for follow-up #2, so the
//! CLI warning "merged via line-based fallback" fires on real merges.

use std::path::Path;

use phantom_core::conflict::{MergeResult, MergeStrategy};
use phantom_core::traits::SemanticAnalyzer;
use phantom_semantic::SemanticMerger;

#[test]
fn unsupported_extension_reports_text_fallback_strategy() {
    let merger = SemanticMerger::new();

    let base = b"alpha\nbeta\ngamma\n";
    let ours = b"alpha\nbeta2\ngamma\n";
    let theirs = b"alpha\nbeta\ngamma\ndelta\n";

    let report = merger
        .three_way_merge(base, ours, theirs, Path::new("config.xyz"))
        .expect("text fallback path must not error");

    assert_eq!(
        report.strategy,
        MergeStrategy::TextFallbackUnsupported,
        "unsupported language must be tagged as text fallback"
    );
    assert!(
        report.strategy.is_text_fallback(),
        "strategy.is_text_fallback() must return true"
    );
    assert!(
        matches!(report.result, MergeResult::Clean(_)),
        "text merger should produce a clean result on non-overlapping edits"
    );
}

#[test]
fn identical_sides_report_trivial_strategy() {
    let merger = SemanticMerger::new();
    let content = b"same content\n";
    let report = merger
        .three_way_merge(content, content, content, Path::new("whatever.xyz"))
        .expect("identical sides must not error");

    assert_eq!(
        report.strategy,
        MergeStrategy::Trivial,
        "short-circuited merge must be tagged Trivial"
    );
    assert!(
        !report.strategy.is_text_fallback(),
        "Trivial is not a text fallback"
    );
}

#[test]
fn supported_language_reports_semantic_strategy() {
    let merger = SemanticMerger::new();
    let base = b"fn a() {}\n";
    let ours = b"fn a() {}\nfn added_by_ours() {}\n";
    let theirs = b"fn a() {}\nfn added_by_theirs() {}\n";

    let report = merger
        .three_way_merge(base, ours, theirs, Path::new("lib.rs"))
        .expect("semantic merge must not error on valid Rust input");

    assert_eq!(
        report.strategy,
        MergeStrategy::Semantic,
        "disjoint Rust edits must be tagged Semantic"
    );
}
