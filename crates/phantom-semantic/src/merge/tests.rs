//! End-to-end tests for [`SemanticMerger::three_way_merge`].
//!
//! These exercise the full merge pipeline (parse → detect conflicts → reconstruct
//! → syntax-validate → fallback). Unit tests for individual stages live alongside
//! each stage's module (`conflict.rs`, `reconstruct.rs`, `text.rs`).

use std::path::Path;

use phantom_core::conflict::{ConflictKind, MergeResult, MergeStrategy};
use phantom_core::traits::SemanticAnalyzer;

use super::SemanticMerger;

fn merger() -> SemanticMerger {
    SemanticMerger::new()
}

/// Run `three_way_merge` and unwrap the report down to the merge result.
/// Keeps the rest of the tests in terms of `MergeResult` rather than the
/// new `MergeReport` wrapper.
fn merge(base: &[u8], ours: &[u8], theirs: &[u8], path: &Path) -> MergeResult {
    merger()
        .three_way_merge(base, ours, theirs, path)
        .unwrap()
        .result
}

/// Variant that returns the strategy alongside the result for tests that
/// need to assert on the fallback classification.
fn merge_with_strategy(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    path: &Path,
) -> (MergeResult, MergeStrategy) {
    let report = merger().three_way_merge(base, ours, theirs, path).unwrap();
    (report.result, report.strategy)
}

#[test]
fn both_add_different_functions_merges_cleanly() {
    let base = b"fn existing() {}\n";
    let ours = b"fn existing() {}\nfn added_by_ours() {}\n";
    let theirs = b"fn existing() {}\nfn added_by_theirs() {}\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("existing"), "should keep existing function");
            assert!(
                text.contains("added_by_ours"),
                "should include ours' addition"
            );
            assert!(
                text.contains("added_by_theirs"),
                "should include theirs' addition"
            );
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn both_modify_same_function_conflicts() {
    let base = b"fn shared() { 1 }\n";
    let ours = b"fn shared() { 2 }\n";
    let theirs = b"fn shared() { 3 }\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Conflict(conflicts) => {
            assert!(!conflicts.is_empty());
            assert!(matches!(
                conflicts[0].kind,
                ConflictKind::BothModifiedSymbol
            ));
        }
        MergeResult::Clean(_) => panic!("expected conflict"),
    }
}

#[test]
fn one_adds_other_modifies_different_function() {
    let base = b"fn original() { 1 }\n";
    let ours = b"fn original() { 2 }\n";
    let theirs = b"fn original() { 1 }\nfn new_fn() {}\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("original"), "should keep modified original");
            assert!(text.contains("new_fn"), "should include new function");
            assert!(text.contains("{ 2 }"), "should use ours' modification");
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn delete_and_modify_same_symbol_conflicts() {
    let base = b"fn shared() { 1 }\nfn other() {}\n";
    let ours = b"fn shared() { 2 }\nfn other() {}\n"; // modified
    let theirs = b"fn other() {}\n"; // deleted

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Conflict(conflicts) => {
            assert!(!conflicts.is_empty());
            assert!(
                conflicts
                    .iter()
                    .any(|c| matches!(c.kind, ConflictKind::ModifyDeleteSymbol))
            );
        }
        MergeResult::Clean(_) => panic!("expected conflict"),
    }
}

#[test]
fn both_add_identical_function_deduplicates() {
    let base = b"fn existing() {}\n";
    let ours = b"fn existing() {}\nfn same_new() { 42 }\n";
    let theirs = b"fn existing() {}\nfn same_new() { 42 }\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("same_new"));
            // Should not duplicate
            let count = text.matches("same_new").count();
            assert_eq!(count, 1, "identical function should appear only once");
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn both_add_same_import_deduplicates() {
    let base = b"fn existing() {}\n";
    let ours = b"use std::io;\nfn existing() {}\n";
    let theirs = b"use std::io;\nfn existing() {}\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("std::io"));
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn disjoint_changes_merge_cleanly() {
    let base = b"fn a() { 1 }\nfn b() { 2 }\n";
    let ours = b"fn a() { 10 }\nfn b() { 2 }\n"; // modified a
    let theirs = b"fn a() { 1 }\nfn b() { 20 }\n"; // modified b

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("{ 10 }"), "should have ours' change to a");
            assert!(text.contains("{ 20 }"), "should have theirs' change to b");
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn unsupported_file_falls_back_to_text_merge() {
    let base = b"line1\nline2\nline3\n";
    let ours = b"line1\nline2_modified\nline3\n";
    let theirs = b"line1\nline2\nline3\nline4\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("config.xyz"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("line2_modified"), "should have ours' change");
            assert!(text.contains("line4"), "should have theirs' addition");
        }
        MergeResult::Conflict(_) => panic!("expected clean text merge"),
    }
}

#[test]
fn identical_ours_and_theirs_returns_clean() {
    let base = b"fn old() {}\n";
    let same = b"fn new_version() {}\n";

    let result = merger()
        .three_way_merge(base, same, same, Path::new("test.rs"))
        .unwrap()
        .result;

    assert!(matches!(result, MergeResult::Clean(_)));
}

#[test]
fn appended_symbols_no_double_newline() {
    let base = b"fn existing() {}\n";
    let ours = b"fn existing() {}\nfn from_ours() {}\n";
    let theirs = b"fn existing() {}\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(
                !text.contains("\n\n\n"),
                "should not have triple newlines, got: {text:?}"
            );
            assert!(text.contains("from_ours"));
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn syntax_validation_catches_broken_merge() {
    let parser = crate::parser::Parser::new();
    let valid = b"fn valid() { 42 }\n";
    let broken = b"fn broken( { 42 }\n";

    assert!(
        !parser.has_syntax_errors(Path::new("test.rs"), valid),
        "valid code should not have errors"
    );
    assert!(
        parser.has_syntax_errors(Path::new("test.rs"), broken),
        "broken code should have errors"
    );
}

#[test]
fn syntax_validation_ignores_unsupported_languages() {
    let parser = crate::parser::Parser::new();
    let content = b"this is { definitely not valid { code";

    assert!(
        !parser.has_syntax_errors(Path::new("config.xyz"), content),
        "unsupported languages should return false (no grammar to check)"
    );
}

// -------- MergeStrategy reporting -----------------------------------------

#[test]
fn short_circuit_tagged_as_trivial() {
    let base = b"fn a() {}\n";
    // ours == theirs → short circuit
    let (result, strategy) = merge_with_strategy(base, base, base, Path::new("test.rs"));
    assert!(matches!(result, MergeResult::Clean(_)));
    assert_eq!(strategy, MergeStrategy::Trivial);
}

#[test]
fn clean_semantic_path_tagged_as_semantic() {
    let base = b"fn a() {}\n";
    let ours = b"fn a() {}\nfn b() {}\n";
    let theirs = b"fn a() {}\nfn c() {}\n";
    let (result, strategy) = merge_with_strategy(base, ours, theirs, Path::new("test.rs"));
    assert!(matches!(result, MergeResult::Clean(_)));
    assert_eq!(strategy, MergeStrategy::Semantic);
}

#[test]
fn unsupported_language_tagged_as_text_fallback() {
    let base = b"line1\nline2\n";
    let ours = b"line1\nline2_modified\n";
    let theirs = b"line1\nline2\nline3\n";
    let (_result, strategy) = merge_with_strategy(base, ours, theirs, Path::new("data.xyz"));
    assert_eq!(strategy, MergeStrategy::TextFallbackUnsupported);
    assert!(strategy.is_text_fallback());
}

#[test]
fn merge_helper_returns_plain_result() {
    // Exercise the `merge` helper so it isn't dead code; also asserts the
    // semantic path still produces a clean merge for disjoint edits.
    let base = b"fn a() { 1 }\n";
    let ours = b"fn a() { 2 }\n";
    let theirs = b"fn a() { 1 }\nfn b() {}\n";
    let result = merge(base, ours, theirs, Path::new("test.rs"));
    assert!(matches!(result, MergeResult::Clean(_)));
}

#[test]
fn new_function_added_after_existing_preserves_order() {
    // Agent adds a function between two existing ones — it should NOT end up at EOF.
    let base = b"fn first() { 1 }\nfn third() { 3 }\n";
    let ours = b"fn first() { 1 }\nfn second() { 2 }\nfn third() { 3 }\n";
    let theirs = base;

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("second"), "should include added function");
            let pos_first = text.find("first").unwrap();
            let pos_second = text.find("second").unwrap();
            let pos_third = text.find("third").unwrap();
            assert!(
                pos_first < pos_second && pos_second < pos_third,
                "functions should be in order: first < second < third, got: {text:?}"
            );
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn new_use_statement_added_before_functions() {
    // Agent adds a use statement at the top — it should appear before functions.
    let base = b"fn existing() { 1 }\n";
    let ours = b"use std::fmt;\nfn existing() { 1 }\n";
    let theirs = base;

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("std::fmt"), "should include use statement");
            let pos_use = text.find("std::fmt").unwrap();
            let pos_fn = text.find("existing").unwrap();
            assert!(
                pos_use < pos_fn,
                "use statement should appear before function, got: {text:?}"
            );
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn both_sides_add_at_different_positions() {
    // Ours adds after first, theirs adds after second.
    let base = b"fn first() { 1 }\nfn last() { 9 }\n";
    let ours = b"fn first() { 1 }\nfn from_ours() { 2 }\nfn last() { 9 }\n";
    let theirs = b"fn first() { 1 }\nfn last() { 9 }\nfn from_theirs() { 8 }\n";

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("from_ours"), "should include ours' addition");
            assert!(
                text.contains("from_theirs"),
                "should include theirs' addition"
            );
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn adjacent_insertions_at_zero_byte_boundary_preserve_order() {
    // One side adds a function after `first` (prepend=false, anchored to prev
    // sibling end) and a use statement before `first` (prepend=true, anchored
    // to next sibling start).  When both anchors resolve to the same byte
    // position, the non-prepend content must appear before the prepend content.
    let base = b"fn first() { 1 }\nfn second() { 2 }\n";
    // Ours adds a helper between first and second, plus a use before first.
    let ours = b"use std::io;\nfn first() { 1 }\nfn helper() { 0 }\nfn second() { 2 }\n";
    let theirs = base;

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("std::io"), "should include use statement");
            assert!(text.contains("helper"), "should include helper function");
            let pos_use = text.find("std::io").unwrap();
            let pos_first = text.find("first").unwrap();
            let pos_helper = text.find("helper").unwrap();
            let pos_second = text.find("second").unwrap();
            assert!(
                pos_use < pos_first && pos_first < pos_helper && pos_helper < pos_second,
                "order should be: use < first < helper < second, got: {text:?}"
            );
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}

#[test]
fn multiple_functions_added_after_same_anchor_preserve_order() {
    // Two new functions added sequentially after the same anchor — their
    // relative order must be preserved in the merged output.
    let base = b"fn anchor() {}\n";
    let ours = b"fn anchor() {}\nfn alpha() {}\nfn beta() {}\n";
    let theirs = base;

    let result = merger()
        .three_way_merge(base, ours, theirs, Path::new("test.rs"))
        .unwrap()
        .result;

    match result {
        MergeResult::Clean(merged) => {
            let text = String::from_utf8_lossy(&merged);
            assert!(text.contains("alpha"), "should include alpha");
            assert!(text.contains("beta"), "should include beta");
            let pos_alpha = text.find("alpha").unwrap();
            let pos_beta = text.find("beta").unwrap();
            assert!(
                pos_alpha < pos_beta,
                "alpha should appear before beta (source order), got: {text:?}"
            );
        }
        MergeResult::Conflict(c) => panic!("expected clean merge, got conflicts: {c:?}"),
    }
}
