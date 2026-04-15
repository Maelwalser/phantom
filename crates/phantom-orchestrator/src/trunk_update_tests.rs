use std::path::PathBuf;

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::{ContentHash, SymbolId};
use phantom_core::notification::TrunkFileStatus;
use phantom_core::symbol::{SymbolEntry, SymbolKind};

use super::*;

fn dummy_symbol(name: &str, kind: SymbolKind, byte_start: usize, byte_end: usize) -> SymbolEntry {
    SymbolEntry {
        id: SymbolId(format!("crate::mod::{name}::{kind:?}")),
        kind,
        name: name.to_string(),
        scope: "crate::mod".to_string(),
        file: PathBuf::from("src/lib.rs"),
        byte_range: byte_start..byte_end,
        content_hash: ContentHash([0; 32]),
    }
}

#[test]
fn render_operation_add_symbol() {
    let sym = dummy_symbol("handle_login", SymbolKind::Function, 100, 200);
    let op = SemanticOperation::AddSymbol {
        file: PathBuf::from("src/lib.rs"),
        symbol: sym,
    };
    // Provide content with 3 newlines before byte 100 → line 4.
    let content = b"line1\nline2\nline3\nfn handle_login() {}";
    let rendered = render_operation(&op, Some(content));
    assert!(rendered.contains("**Added**"));
    assert!(rendered.contains("`handle_login()`"));
    assert!(rendered.contains("Function"));
}

#[test]
fn render_operation_delete_symbol() {
    let op = SemanticOperation::DeleteSymbol {
        file: PathBuf::from("src/lib.rs"),
        id: SymbolId("crate::mod::old_fn::Function".to_string()),
    };
    let rendered = render_operation(&op, None);
    assert!(rendered.contains("**Deleted**"));
    assert!(rendered.contains("`old_fn()`"));
}

#[test]
fn render_operation_raw_diff() {
    let op = SemanticOperation::RawDiff {
        path: PathBuf::from("config.toml"),
        patch: String::new(),
    };
    let rendered = render_operation(&op, None);
    assert!(rendered.contains("Raw changes"));
}

#[test]
fn status_labels_are_readable() {
    assert!(status_label(TrunkFileStatus::TrunkVisible).contains("trunk visible"));
    assert!(status_label(TrunkFileStatus::Shadowed).contains("shadowed"));
    assert!(status_label(TrunkFileStatus::RebaseMerged).contains("merged cleanly"));
    assert!(status_label(TrunkFileStatus::RebaseConflict).contains("CONFLICT"));
}

#[test]
fn byte_offset_to_line_basic() {
    let content = b"aaa\nbbb\nccc\n";
    assert_eq!(byte_offset_to_line(content, 0), 1); // start of file
    assert_eq!(byte_offset_to_line(content, 4), 2); // after first \n
    assert_eq!(byte_offset_to_line(content, 8), 3); // after second \n
}

#[test]
fn byte_offset_to_line_beyond_content() {
    let content = b"ab\ncd\n";
    // Offset past the end should clamp.
    assert_eq!(byte_offset_to_line(content, 100), 3);
}

#[test]
fn write_creates_new_file() {
    let dir = tempfile::tempdir().unwrap();
    write_trunk_update_md(dir.path(), "# Update 1\n").unwrap();
    let content = std::fs::read_to_string(dir.path().join(TRUNK_UPDATE_FILE)).unwrap();
    assert!(content.starts_with("# Update 1"));
}

#[test]
fn write_appends_with_separator() {
    let dir = tempfile::tempdir().unwrap();
    write_trunk_update_md(dir.path(), "# Update 1\n").unwrap();
    write_trunk_update_md(dir.path(), "# Update 2\n").unwrap();
    let content = std::fs::read_to_string(dir.path().join(TRUNK_UPDATE_FILE)).unwrap();
    assert!(content.contains("# Update 1"));
    assert!(content.contains("---"));
    assert!(content.contains("# Update 2"));
}

#[test]
fn remove_cleans_up_file() {
    let dir = tempfile::tempdir().unwrap();
    write_trunk_update_md(dir.path(), "# Update\n").unwrap();
    assert!(dir.path().join(TRUNK_UPDATE_FILE).exists());
    remove_trunk_update_md(dir.path());
    assert!(!dir.path().join(TRUNK_UPDATE_FILE).exists());
}

#[test]
fn remove_noop_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    // Should not panic or error.
    remove_trunk_update_md(dir.path());
}

#[test]
fn write_appends_to_context_file_when_present() {
    let dir = tempfile::tempdir().unwrap();
    // Create a context file with a static preamble.
    let preamble = "# Phantom Agent Session\n\n## Commands\n- submit\n\n---\n\n## Trunk Updates\n";
    std::fs::write(dir.path().join(CONTEXT_FILE), preamble).unwrap();

    write_trunk_update_md(dir.path(), "# Update 1\n").unwrap();

    let content = std::fs::read_to_string(dir.path().join(CONTEXT_FILE)).unwrap();
    // Static preamble preserved.
    assert!(content.starts_with(preamble));
    // Update appended.
    assert!(content.contains("# Update 1"));
}

#[test]
fn write_skips_context_file_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    // No context file — should not create one.
    write_trunk_update_md(dir.path(), "# Update 1\n").unwrap();

    assert!(!dir.path().join(CONTEXT_FILE).exists());
    // But the trunk update file should still be created.
    assert!(dir.path().join(TRUNK_UPDATE_FILE).exists());
}

#[test]
fn kind_label_covers_all_variants() {
    // Ensure every SymbolKind variant has a non-empty label.
    let kinds = [
        SymbolKind::Function,
        SymbolKind::Struct,
        SymbolKind::Enum,
        SymbolKind::Trait,
        SymbolKind::Impl,
        SymbolKind::Import,
        SymbolKind::Const,
        SymbolKind::TypeAlias,
        SymbolKind::Module,
        SymbolKind::Test,
        SymbolKind::Class,
        SymbolKind::Interface,
        SymbolKind::Method,
        SymbolKind::Section,
        SymbolKind::Directive,
        SymbolKind::Variable,
    ];
    for kind in kinds {
        assert!(!kind_label(kind).is_empty());
    }
}
