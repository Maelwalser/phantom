use super::diff_util::line_to_byte_offset;
use super::formats::compact_symbol::write_compact_conflict;
use super::formats::compact_text::write_compact_raw_text_conflict;
use super::formats::diff3_markers::build_conflict_marker_view;
use super::formats::minimal::write_minimal_fallback;
use super::truncate::{compute_truncation_center, first_divergence_offset, write_truncated};
use super::*;

// Re-expose the private constant used in one test so the test body stays
// readable; duplicating the number here would silently drift if the real
// budget were tuned.
const WHOLE_FILE_BYTE_BUDGET: usize = 32_768;

#[test]
fn write_truncated_under_budget_is_identity() {
    let text = "fn main() {}\n";
    let mut out = String::new();
    write_truncated(&mut out, text, 0);
    assert_eq!(out, text);
}

#[test]
fn write_truncated_over_budget_cuts_at_line() {
    // Build a string larger than WHOLE_FILE_BYTE_BUDGET.
    let line = "x".repeat(100) + "\n";
    let count = (WHOLE_FILE_BYTE_BUDGET / line.len()) + 10;
    let text: String = line.repeat(count);
    assert!(text.len() > WHOLE_FILE_BYTE_BUDGET);

    let mut out = String::new();
    write_truncated(&mut out, &text, 0);
    assert!(out.len() < text.len());
    assert!(out.contains("CONTENT TRUNCATED"));
    assert!(out.contains("more tokens below"));
}

#[test]
fn first_divergence_offset_identical() {
    assert_eq!(first_divergence_offset("hello", "hello"), None);
}

#[test]
fn first_divergence_offset_at_position() {
    assert_eq!(first_divergence_offset("abcdef", "abcXef"), Some(3));
}

#[test]
fn first_divergence_offset_length_diff() {
    assert_eq!(first_divergence_offset("abc", "abcdef"), Some(3));
    assert_eq!(first_divergence_offset("abcdef", "abc"), Some(3));
}

#[test]
fn compute_truncation_center_picks_earliest() {
    let base = "aaaa_bbbb_cccc_dddd";
    // Diverges from base at position 5
    let ours = "aaaa_XXXX_cccc_dddd";
    // Diverges from base at position 10
    let theirs = "aaaa_bbbb_YYYY_dddd";
    let center = compute_truncation_center(Some(base), Some(ours), Some(theirs));
    assert_eq!(center, 5);
}

#[test]
fn compute_truncation_center_no_content_returns_zero() {
    assert_eq!(compute_truncation_center(None, None, None), 0);
}

#[test]
fn write_truncated_centers_on_offset() {
    // Build a ~100KB file with a distinctive marker near byte 70,000.
    let prefix = "prefix_line\n".repeat(5000); // ~60,000 bytes
    let marker = "CONFLICT_MARKER_HERE\n";
    let suffix = "suffix_line\n".repeat(5000); // ~60,000 bytes
    let text = format!("{prefix}{marker}{suffix}");
    assert!(text.len() > WHOLE_FILE_BYTE_BUDGET * 2);

    let center = prefix.len() + 10; // points into the marker area
    let mut out = String::new();
    write_truncated(&mut out, &text, center);

    // The output should contain the marker.
    assert!(
        out.contains("CONFLICT_MARKER_HERE"),
        "centered truncation should include the conflict region"
    );
    // Should have a leading truncation marker (since center is far from byte 0).
    assert!(
        out.contains("lines above"),
        "should have leading truncation marker"
    );
    // Should have a trailing truncation marker.
    assert!(
        out.contains("more tokens below"),
        "should have trailing truncation marker"
    );
    // Should NOT start from the very beginning of the file.
    assert!(
        !out.starts_with("prefix_line"),
        "should not start from byte 0 when center is far away"
    );
}

#[test]
fn fallback_uses_conflict_markers_instead_of_three_blocks() {
    // build_conflict_marker_view should produce diff3 markers for divergent content.
    let base = "line1\nline2\nline3\n";
    let ours = "line1\nours_change\nline3\n";
    let theirs = "line1\ntheirs_change\nline3\n";

    let merged = build_conflict_marker_view(Some(base), Some(ours), Some(theirs));
    assert!(merged.is_some(), "should produce merged output");
    let merged = merged.unwrap();
    // Should contain conflict markers, not three separate blocks.
    assert!(merged.contains("<<<<<<<"), "should contain <<<<<<< marker");
    assert!(merged.contains(">>>>>>>"), "should contain >>>>>>> marker");
    assert!(merged.contains("======="), "should contain ======= marker");
    assert!(
        merged.contains("|||||||"),
        "should contain ||||||| marker (diff3 style)"
    );
    // Shared context should appear only once.
    assert_eq!(
        merged.matches("line1").count(),
        1,
        "shared line should appear once"
    );
    assert_eq!(
        merged.matches("line3").count(),
        1,
        "shared line should appear once"
    );
    // Both changes should be present.
    assert!(merged.contains("ours_change"));
    assert!(merged.contains("theirs_change"));
}

#[test]
fn fallback_conflict_markers_returns_none_when_content_missing() {
    assert!(build_conflict_marker_view(None, Some("a"), Some("b")).is_none());
    assert!(build_conflict_marker_view(Some("a"), None, Some("b")).is_none());
    assert!(build_conflict_marker_view(Some("a"), Some("b"), None).is_none());
}

#[test]
fn fallback_conflict_markers_clean_merge() {
    // Non-overlapping changes should produce a clean merge (no markers).
    let base = "line1\nline2\nline3\n";
    let ours = "OURS\nline2\nline3\n";
    let theirs = "line1\nline2\nTHEIRS\n";

    let merged = build_conflict_marker_view(Some(base), Some(ours), Some(theirs));
    let merged = merged.unwrap();
    assert!(
        !merged.contains("<<<<<<<"),
        "clean merge should have no conflict markers"
    );
    assert!(merged.contains("OURS"), "ours change should be integrated");
    assert!(
        merged.contains("THEIRS"),
        "theirs change should be integrated"
    );
}

#[test]
fn minimal_fallback_picks_best_content() {
    let mut out = String::new();
    write_minimal_fallback(
        &mut out,
        "txt",
        None,
        None,
        Some("theirs content"),
        "abc",
        0,
    );
    assert!(out.contains("THEIRS"), "should prefer theirs");
    assert!(out.contains("theirs content"));

    let mut out = String::new();
    write_minimal_fallback(&mut out, "txt", None, Some("ours content"), None, "abc", 0);
    assert!(out.contains("OURS"), "should fall back to ours");

    let mut out = String::new();
    write_minimal_fallback(&mut out, "txt", Some("base content"), None, None, "abc", 0);
    assert!(out.contains("BASE"), "should fall back to base");

    let mut out = String::new();
    write_minimal_fallback(&mut out, "txt", None, None, None, "abc", 0);
    assert!(
        out.contains("no file content available"),
        "should show message when all missing"
    );
}

fn make_both_modified_conflict(
    base_src: &str,
    ours_src: &str,
    theirs_src: &str,
) -> ResolveConflictContext {
    use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
    use phantom_core::id::{ChangesetId, SymbolId};

    // Compute a span inside the first function body so that
    // `find_enclosing_symbol` can locate it. We point at a byte
    // range strictly within the function, not at the trailing newline.
    let span_of = |src: &str| {
        let start = src.find("fn ").unwrap_or(0);
        let end = src.rfind('}').map_or(src.len(), |p| p + 1);
        ConflictSpan::from_byte_range(src.as_bytes(), start..end)
    };

    ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::BothModifiedSymbol,
            file: std::path::PathBuf::from("src/handler.rs"),
            symbol_id: Some(SymbolId("crate::handler::target::function".into())),
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "both sides modified handler::target".into(),
            base_span: Some(span_of(base_src)),
            ours_span: Some(span_of(ours_src)),
            theirs_span: Some(span_of(theirs_src)),
        },
        base_content: Some(base_src.to_string()),
        ours_content: Some(ours_src.to_string()),
        theirs_content: Some(theirs_src.to_string()),
    }
}

#[test]
fn compact_format_for_both_modified_symbol() {
    let base = "fn target() {\n    let x = 1;\n    let y = 2;\n}\n";
    let ours = "fn target() {\n    let x = 10;\n    let y = 2;\n}\n";
    let theirs = "fn target() {\n    let x = 1;\n    let y = 20;\n}\n";

    let conflict = make_both_modified_conflict(base, ours, theirs);
    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc123", &parser);

    assert!(ok, "should succeed for BothModifiedSymbol with all content");
    // BASE shown once as a code block.
    assert!(out.contains("#### BASE"));
    assert!(out.contains("```rust"));
    assert!(out.contains("fn target()"));
    // OURS and THEIRS shown as diffs.
    assert!(out.contains("#### OURS"));
    assert!(out.contains("#### THEIRS"));
    assert!(out.contains("```diff"));
    // Should NOT contain three full code blocks.
    let rust_block_count = out.matches("```rust").count();
    assert_eq!(rust_block_count, 1, "BASE should be the only rust block");
    // Redundant diff headers should be stripped for token efficiency.
    assert!(
        !out.contains("--- original"),
        "diff header '--- original' should be stripped"
    );
    assert!(
        !out.contains("+++ modified"),
        "diff header '+++ modified' should be stripped"
    );
    // Hunk headers should be preserved.
    assert!(out.contains("@@"), "hunk headers should be preserved");
    // Scope context header should appear between BASE and the diffs.
    assert!(
        out.contains("#### Scope Context"),
        "scope context header should be present"
    );
    assert!(
        out.contains("`fn target() {`"),
        "scope signature should be the function signature"
    );
}

#[test]
fn compact_format_falls_back_when_content_missing() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
    use phantom_core::id::ChangesetId;

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::BothModifiedSymbol,
            file: std::path::PathBuf::from("src/lib.rs"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "conflict".into(),
            base_span: Some(ConflictSpan {
                byte_range: 0..10,
                start_line: 1,
                end_line: 1,
            }),
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some("fn foo() {}".into()),
        ours_content: None, // missing
        theirs_content: Some("fn foo() { 1 }".into()),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc", &parser);
    assert!(!ok, "should fall back when ours_content is missing");
}

#[test]
fn compact_format_identical_side_shows_message() {
    let base = "fn target() {\n    let x = 1;\n}\n";
    let ours = base; // identical
    let theirs = "fn target() {\n    let x = 99;\n}\n";

    let conflict = make_both_modified_conflict(base, ours, theirs);
    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc", &parser);

    assert!(ok);
    assert!(
        out.contains("*(identical to BASE)*"),
        "OURS should show identical message"
    );
    // THEIRS should still show a diff.
    assert!(out.contains("```diff"));
}

#[test]
fn compact_format_deleted_side() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
    use phantom_core::id::{ChangesetId, SymbolId};

    let base = "fn target() {\n    let x = 1;\n}\n";
    let ours = "fn target() {\n    let x = 10;\n}\n"; // modified
    // theirs deleted the symbol — file still exists but symbol is gone
    let theirs = "// empty\n";

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::ModifyDeleteSymbol,
            file: std::path::PathBuf::from("src/handler.rs"),
            symbol_id: Some(SymbolId("crate::handler::target::function".into())),
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "ours modified target but theirs deleted it".into(),
            base_span: Some(ConflictSpan::from_byte_range(
                base.as_bytes(),
                0..base.rfind('}').unwrap() + 1,
            )),
            ours_span: Some(ConflictSpan::from_byte_range(
                ours.as_bytes(),
                0..ours.rfind('}').unwrap() + 1,
            )),
            theirs_span: None, // deleted
        },
        base_content: Some(base.into()),
        ours_content: Some(ours.into()),
        theirs_content: Some(theirs.into()),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_conflict(&mut out, "rust", &conflict, "abc", &parser);
    assert!(ok);
    assert!(
        out.contains("*(symbol deleted)*"),
        "THEIRS should show deleted message"
    );
}

#[test]
fn full_resolve_file_uses_compact_for_symbol_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("test".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
    let base_commit = phantom_core::id::GitOid([0u8; 20]);

    let base = "fn target() {\n    let x = 1;\n    let y = 2;\n}\n";
    let ours = "fn target() {\n    let x = 10;\n    let y = 2;\n}\n";
    let theirs = "fn target() {\n    let x = 1;\n    let y = 20;\n}\n";

    let conflicts = vec![make_both_modified_conflict(base, ours, theirs)];

    write_resolve_context_file(
        dir.path(),
        &agent_id,
        &changeset_id,
        &base_commit,
        &conflicts,
        None,
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    // Should use compact format: one rust block + diff blocks.
    assert!(content.contains("```diff"), "should contain diff blocks");
    let rust_blocks = content.matches("```rust").count();
    assert_eq!(
        rust_blocks, 1,
        "should have exactly one rust code block (BASE)"
    );
}

#[test]
fn full_resolve_file_uses_compact_for_raw_text() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("test".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
    let base_commit = phantom_core::id::GitOid([0u8; 20]);

    let conflicts = vec![ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: std::path::PathBuf::from("config.toml"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "text conflict".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some("key = 1\n".into()),
        ours_content: Some("key = 2\n".into()),
        theirs_content: Some("key = 3\n".into()),
    }];

    write_resolve_context_file(
        dir.path(),
        &agent_id,
        &changeset_id,
        &base_commit,
        &conflicts,
        None,
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    // Should use compact diff format — OURS/THEIRS as diffs, no BASE block.
    assert!(
        content.contains("```diff"),
        "RawTextConflict should use diff format"
    );
    assert!(
        !content.contains("#### BASE"),
        "raw text conflicts should not emit a BASE block"
    );
    assert!(content.contains("#### OURS"));
    assert!(content.contains("#### THEIRS"));
    assert_eq!(
        content.matches("```toml").count(),
        0,
        "no toml code blocks — only diffs"
    );
}

#[test]
fn compact_format_for_raw_text_conflict() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    let base = "line1\nline2\nline3\n";
    let ours = "line1\nmodified_ours\nline3\n";
    let theirs = "line1\nline2\nmodified_theirs\n";

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: std::path::PathBuf::from("README.md"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "raw text conflict in README".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some(base.into()),
        ours_content: Some(ours.into()),
        theirs_content: Some(theirs.into()),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_raw_text_conflict(&mut out, "markdown", &conflict, "abc123", &parser);

    assert!(ok, "should succeed for RawTextConflict with all content");
    assert!(
        !out.contains("#### BASE"),
        "raw text conflicts should not emit a BASE block"
    );
    assert!(
        !out.contains("```markdown"),
        "no markdown code block — only diffs"
    );
    // Non-parseable file (README.md) should NOT emit scope context.
    assert!(
        !out.contains("#### Scope Context"),
        "non-parseable file should not have scope context"
    );
    assert!(out.contains("#### OURS"));
    assert!(out.contains("#### THEIRS"));
    assert!(out.contains("```diff"));
    // Diffs should contain the actual changes.
    assert!(out.contains("modified_ours"));
    assert!(out.contains("modified_theirs"));
}

#[test]
fn compact_format_for_dependency_version_conflict() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    let base = "[dependencies]\nfoo = \"1.0\"\n";
    let ours = "[dependencies]\nfoo = \"1.1\"\n";
    let theirs = "[dependencies]\nfoo = \"2.0\"\n";

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::BothModifiedDependencyVersion,
            file: std::path::PathBuf::from("Cargo.toml"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "both modified foo version".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some(base.into()),
        ours_content: Some(ours.into()),
        theirs_content: Some(theirs.into()),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_raw_text_conflict(&mut out, "toml", &conflict, "abc123", &parser);

    assert!(ok, "should succeed for BothModifiedDependencyVersion");
    assert!(out.contains("```diff"));
    assert!(out.contains("1.1"));
    assert!(out.contains("2.0"));
}

#[test]
fn raw_text_compact_falls_back_when_content_missing() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: std::path::PathBuf::from("config.yaml"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "conflict".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some("key: value\n".into()),
        ours_content: None, // missing
        theirs_content: Some("key: other\n".into()),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_raw_text_conflict(&mut out, "yaml", &conflict, "abc", &parser);
    assert!(!ok, "should fall back when ours_content is missing");
}

#[test]
fn raw_text_identical_side_shows_message() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    let base = "line1\nline2\n";
    let ours = base; // identical
    let theirs = "line1\nchanged\n";

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: std::path::PathBuf::from("notes.md"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "raw text conflict".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some(base.into()),
        ours_content: Some(ours.into()),
        theirs_content: Some(theirs.into()),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_raw_text_conflict(&mut out, "markdown", &conflict, "abc", &parser);
    assert!(ok);
    assert!(
        out.contains("*(identical to BASE)*"),
        "OURS should show identical message"
    );
    assert!(out.contains("```diff"), "THEIRS should show a diff");
}

#[test]
fn raw_text_compact_emits_scope_for_parseable_file() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    // A Rust file with two functions — the change is deep inside `handler`.
    let base = "\
fn setup() {
    init();
}

fn handler(req: Request) -> Response {
    let a = 1;
    let b = 2;
    let c = 3;
    let d = 4;
    let e = 5;
    let f = 6;
    let result = a + b + c + d + e + f;
    respond(result)
}
";
    // OURS modifies a line deep in handler.
    let ours = base.replace(
        "let result = a + b + c + d + e + f;",
        "let result = a + b + c;",
    );
    // THEIRS modifies a different line in handler.
    let theirs = base.replace("respond(result)", "respond(result * 2)");

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: std::path::PathBuf::from("src/main.rs"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "raw text conflict in Rust file".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some(base.into()),
        ours_content: Some(ours),
        theirs_content: Some(theirs),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_raw_text_conflict(&mut out, "rust", &conflict, "abc123", &parser);

    assert!(ok, "should succeed");
    // Scope context should be emitted for the parseable Rust file.
    assert!(
        out.contains("#### Scope Context"),
        "scope context header should be present"
    );
    assert!(
        out.contains("`fn handler(req: Request) -> Response {`"),
        "scope signature should identify the enclosing function, got:\n{out}"
    );
    // setup() should NOT appear anywhere — not in scope context, not in diffs.
    assert!(
        !out.contains("`fn setup()"),
        "unrelated function should not appear in scope context"
    );
    assert!(
        !out.contains("init()"),
        "diff should be scoped to handler — setup/init should not leak into output"
    );
}

#[test]
fn raw_text_scoped_diffs_for_two_functions() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    // A file with three functions; changes touch two of them.
    let base = "\
fn untouched() {
    noop();
}

fn alpha() {
    let x = 1;
}

fn beta() {
    let y = 2;
}
";
    let ours = base.replace("let x = 1;", "let x = 10;");
    let theirs = base.replace("let y = 2;", "let y = 20;");

    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: std::path::PathBuf::from("src/lib.rs"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "changes in alpha and beta".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some(base.into()),
        ours_content: Some(ours),
        theirs_content: Some(theirs),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_raw_text_conflict(&mut out, "rust", &conflict, "abc123", &parser);

    assert!(ok, "should succeed");
    // Two scope context headers — one for alpha, one for beta.
    assert!(out.contains("`fn alpha() {`"), "should scope to alpha");
    assert!(out.contains("`fn beta() {`"), "should scope to beta");
    // untouched() should not appear anywhere.
    assert!(
        !out.contains("untouched"),
        "unmodified function should not appear in scoped output"
    );
    assert!(
        !out.contains("noop"),
        "body of unmodified function should not appear"
    );
}

#[test]
fn resolve_rules_file_contains_all_rules() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules.md");
    write_resolve_rules_file(&path).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    // All 9 rules present.
    for i in 1..=9 {
        assert!(content.contains(&format!("{i}.")), "missing rule {i}");
    }
    assert!(content.contains("After Resolution"));
    assert!(content.contains("automatically submitted and materialized"));
}

#[test]
fn raw_text_compact_falls_back_for_oversized_content() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    let big = "x".repeat(300_000);
    let conflict = ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: std::path::PathBuf::from("package-lock.json"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "conflict in large file".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some(big.clone()),
        ours_content: Some(big.clone()),
        theirs_content: Some(big),
    };

    let mut out = String::new();
    let parser = phantom_semantic::Parser::new();
    let ok = write_compact_raw_text_conflict(&mut out, "json", &conflict, "abc", &parser);
    assert!(!ok, "should fall back for oversized content (>250KB)");
}

#[test]
fn symbol_conflict_cascades_to_raw_text_on_parse_failure() {
    use phantom_core::conflict::{ConflictDetail, ConflictKind};
    use phantom_core::id::ChangesetId;

    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("test".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
    let base_commit = phantom_core::id::GitOid([0u8; 20]);

    let base = "key = 1\n";
    let ours = "key = 2\n";
    let theirs = "key = 3\n";

    // Use BothModifiedSymbol with an unsupported file extension so
    // write_compact_conflict fails, then verify raw text diff is used.
    let conflicts = vec![ResolveConflictContext {
        detail: ConflictDetail {
            kind: ConflictKind::BothModifiedSymbol,
            file: std::path::PathBuf::from("config.unknown"),
            symbol_id: None,
            ours_changeset: ChangesetId("cs-1".into()),
            theirs_changeset: ChangesetId("cs-2".into()),
            description: "both modified config".into(),
            base_span: None,
            ours_span: None,
            theirs_span: None,
        },
        base_content: Some(base.into()),
        ours_content: Some(ours.into()),
        theirs_content: Some(theirs.into()),
    }];

    write_resolve_context_file(
        dir.path(),
        &agent_id,
        &changeset_id,
        &base_commit,
        &conflicts,
        None,
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    // Should cascade to raw text diff, not the three-block fallback.
    assert!(
        content.contains("```diff"),
        "should cascade to raw text diff"
    );
    assert!(
        !content.contains("#### BASE"),
        "should not fall through to three-block dump"
    );
}

#[test]
fn resolve_context_file_excludes_rules() {
    let dir = tempfile::tempdir().unwrap();
    let agent_id = phantom_core::id::AgentId("test".to_string());
    let changeset_id = phantom_core::id::ChangesetId("cs-1".to_string());
    let base_commit = phantom_core::id::GitOid([0u8; 20]);

    write_resolve_context_file(
        dir.path(),
        &agent_id,
        &changeset_id,
        &base_commit,
        &[],
        None,
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    assert!(content.contains("Phantom Conflict Resolution"));
    assert!(content.contains("Agent: test"));
    // Rules should NOT be in this file — they live in the system prompt.
    assert!(!content.contains("Resolution Rules"));
    assert!(!content.contains("After Resolution"));
}

#[test]
fn line_to_byte_offset_basics() {
    let content = "aaa\nbbb\nccc\n";
    // Line 1 starts at byte 0.
    assert_eq!(line_to_byte_offset(content, 1), 0);
    // Line 2 starts at byte 4 (after "aaa\n").
    assert_eq!(line_to_byte_offset(content, 2), 4);
    // Line 3 starts at byte 8 (after "aaa\nbbb\n").
    assert_eq!(line_to_byte_offset(content, 3), 8);
    // Line 0 or below clamps to 0.
    assert_eq!(line_to_byte_offset(content, 0), 0);
    // Line beyond content returns content.len().
    assert_eq!(line_to_byte_offset(content, 100), content.len());
}
