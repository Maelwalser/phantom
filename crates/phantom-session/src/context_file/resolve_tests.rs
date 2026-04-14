use super::*;

#[test]
fn truncate_to_token_budget_under_budget_is_identity() {
    let text = "fn main() {}\n";
    assert_eq!(truncate_to_token_budget(text), text);
}

#[test]
fn truncate_to_token_budget_over_budget_cuts_at_line() {
    // Build a string larger than WHOLE_FILE_BYTE_BUDGET.
    let line = "x".repeat(100) + "\n";
    let count = (WHOLE_FILE_BYTE_BUDGET / line.len()) + 10;
    let text: String = line.repeat(count);
    assert!(text.len() > WHOLE_FILE_BYTE_BUDGET);

    let result = truncate_to_token_budget(&text);
    assert!(result.len() < text.len());
    assert!(result.contains("// ... truncated (~"));
    assert!(result.contains("more tokens)"));
    // The cut should be at a newline boundary — no partial lines.
    let before_comment = result.split("// ... truncated").next().unwrap();
    assert!(before_comment.ends_with('\n'));
}

#[test]
fn extract_span_context_uses_semantic_for_rust() {
    let src = "struct Foo {}\n\nfn target() {\n    let x = 1;\n    let y = 2;\n}\n\nfn other() {}\n";
    let span = phantom_core::conflict::ConflictSpan {
        byte_range: 28..39, // inside "fn target()"
        start_line: 4,
        end_line: 4,
    };
    let parser = phantom_semantic::Parser::new();
    let result = extract_span_context(src, &span, Path::new("test.rs"), &parser);
    // Should return the entire fn target() body, not just ±10 lines.
    assert!(result.contains("fn target()"));
    assert!(result.contains("let x = 1;"));
    assert!(result.contains("let y = 2;"));
    // Should NOT include unrelated symbols.
    assert!(!result.contains("struct Foo"));
    assert!(!result.contains("fn other"));
}

#[test]
fn extract_span_context_falls_back_for_unsupported_lang() {
    let src = "line1\nline2\nline3\nline4\nline5\n";
    let span = phantom_core::conflict::ConflictSpan {
        byte_range: 6..11,
        start_line: 2,
        end_line: 2,
    };
    let parser = phantom_semantic::Parser::new();
    let result = extract_span_context(src, &span, Path::new("config.toml"), &parser);
    // Fallback path: should include surrounding lines.
    assert!(result.contains("line1"));
    assert!(result.contains("line2"));
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
        let end = src.rfind('}').map(|p| p + 1).unwrap_or(src.len());
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
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    // Should use compact format: one rust block + diff blocks.
    assert!(content.contains("```diff"), "should contain diff blocks");
    let rust_blocks = content.matches("```rust").count();
    assert_eq!(rust_blocks, 1, "should have exactly one rust code block (BASE)");
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
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    // Should use compact diff format — OURS/THEIRS as diffs, no BASE block.
    assert!(
        content.contains("```diff"),
        "RawTextConflict should use diff format"
    );
    assert!(!content.contains("#### BASE"), "raw text conflicts should not emit a BASE block");
    assert!(content.contains("#### OURS"));
    assert!(content.contains("#### THEIRS"));
    assert_eq!(content.matches("```toml").count(), 0, "no toml code blocks — only diffs");
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
    let ok = write_compact_raw_text_conflict(&mut out, "markdown", &conflict, "abc123");

    assert!(ok, "should succeed for RawTextConflict with all content");
    assert!(!out.contains("#### BASE"), "raw text conflicts should not emit a BASE block");
    assert!(!out.contains("```markdown"), "no markdown code block — only diffs");
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
    let ok = write_compact_raw_text_conflict(&mut out, "toml", &conflict, "abc123");

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
    let ok = write_compact_raw_text_conflict(&mut out, "yaml", &conflict, "abc");
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
    let ok = write_compact_raw_text_conflict(&mut out, "markdown", &conflict, "abc");
    assert!(ok);
    assert!(
        out.contains("*(identical to BASE)*"),
        "OURS should show identical message"
    );
    assert!(out.contains("```diff"), "THEIRS should show a diff");
}

#[test]
fn resolve_rules_file_contains_all_rules() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rules.md");
    write_resolve_rules_file(&path).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    // All 9 rules present.
    for i in 1..=9 {
        assert!(
            content.contains(&format!("{}.", i)),
            "missing rule {i}"
        );
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
    let ok = write_compact_raw_text_conflict(&mut out, "json", &conflict, "abc");
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
    )
    .unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    assert!(content.contains("Phantom Conflict Resolution"));
    assert!(content.contains("Agent: test"));
    // Rules should NOT be in this file — they live in the system prompt.
    assert!(!content.contains("Resolution Rules"));
    assert!(!content.contains("After Resolution"));
}
