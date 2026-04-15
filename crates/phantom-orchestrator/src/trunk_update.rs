//! Active Semantic Notifications — structured markdown trunk update files.
//!
//! After a changeset is materialized and ripple effects are processed, this
//! module generates a human-readable `.phantom-trunk-update.md` file in each
//! affected agent's upper directory. The file describes which symbols were
//! added, modified, or deleted, giving the agent structured awareness of
//! trunk changes without requiring it to re-read files or spend tool calls.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::notification::TrunkFileStatus;
use phantom_core::symbol::SymbolKind;

use crate::git::GitOps;

/// Name of the markdown notification file placed in agent overlays.
pub const TRUNK_UPDATE_FILE: &str = ".phantom-trunk-update.md";

/// Name of the context file. Updates are appended to its dynamic section to
/// keep the static preamble byte-identical for prompt cache efficiency.
const CONTEXT_FILE: &str = ".phantom-task.md";

/// Maximum byte size for the generated markdown (keeps token budget bounded).
const BYTE_BUDGET: usize = 4096;

/// Generate markdown notification content from semantic operations and
/// classified file statuses.
///
/// Operations are filtered to only those affecting `classified_files`. Line
/// numbers are computed from the source at `head` when available.
pub fn generate_trunk_update_md(
    submitting_agent: &AgentId,
    changeset_id: &ChangesetId,
    head: &GitOid,
    operations: &[SemanticOperation],
    classified_files: &[(PathBuf, TrunkFileStatus)],
    git: &GitOps,
) -> String {
    let head_short = &head.to_hex()[..12.min(head.to_hex().len())];

    let mut md = format!(
        "# Trunk Update\n\n\
         Agent `{submitting_agent}` submitted changeset `{changeset_id}` \
         (commit `{head_short}`).\n\
         Changes affecting your working files:\n",
    );

    // Group operations by file path (BTreeMap for deterministic ordering).
    let mut ops_by_file: BTreeMap<&Path, Vec<&SemanticOperation>> = BTreeMap::new();
    for op in operations {
        ops_by_file.entry(op.file_path()).or_default().push(op);
    }

    let files_total = classified_files.len();

    for (i, (file, status)) in classified_files.iter().enumerate() {
        let section = render_file_section(file, status, ops_by_file.get(file.as_path()), head, git);

        if md.len() + section.len() > BYTE_BUDGET {
            let remaining = files_total - i;
            if remaining > 0 {
                use std::fmt::Write;
                let _ = write!(md, "\n... and {remaining} more file(s) affected.\n");
            }
            break;
        }

        md.push_str(&section);
    }

    md.push_str(
        "\n---\n*Your overlay has been live-rebased where applicable. \
         No action needed unless you depend on the modified symbols' behavior.*\n",
    );

    md
}

/// Write (or append) the markdown notification into an agent's upper directory.
///
/// If `.phantom-trunk-update.md` already exists (from a prior submit by a
/// different agent), appends with a horizontal rule separator to preserve
/// the timeline.
///
/// Also appends the update to `.phantom-task.md` (the agent's context file)
/// so that the agent sees trunk changes inline without reading a second file.
/// The context file's static preamble remains byte-identical at the top,
/// maximising prompt cache hits.
pub fn write_trunk_update_md(upper_dir: &Path, content: &str) -> std::io::Result<()> {
    // Write to the dedicated trunk update file (backward-compatible).
    let path = upper_dir.join(TRUNK_UPDATE_FILE);
    if path.exists() {
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        use std::io::Write;
        write!(file, "\n---\n\n{content}")?;
    } else {
        std::fs::write(&path, content)?;
    }

    // Append to the context file's dynamic section (if it exists).
    append_to_context_file(upper_dir, content)?;

    Ok(())
}

/// Append an update to the context file's dynamic `## Trunk Updates` section.
///
/// If the context file does not exist (agent created before this feature, or
/// the context file was cleaned up), this is a no-op.
fn append_to_context_file(upper_dir: &Path, content: &str) -> std::io::Result<()> {
    let path = upper_dir.join(CONTEXT_FILE);
    if !path.exists() {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
    use std::io::Write;
    write!(file, "\n---\n\n{content}")?;

    Ok(())
}

/// Remove a stale trunk update markdown file if it exists.
pub fn remove_trunk_update_md(upper_dir: &Path) {
    let path = upper_dir.join(TRUNK_UPDATE_FILE);
    let _ = std::fs::remove_file(path);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Render a markdown section for a single file.
fn render_file_section(
    file: &Path,
    status: &TrunkFileStatus,
    ops: Option<&Vec<&SemanticOperation>>,
    head: &GitOid,
    git: &GitOps,
) -> String {
    let status_label = status_label(status);
    let mut section = format!("\n## {} [{}]\n\n", file.display(), status_label);

    let Some(ops) = ops else {
        section.push_str("- File changed (no semantic detail available)\n");
        return section;
    };

    if ops.is_empty() {
        section.push_str("- File changed (no semantic detail available)\n");
        return section;
    }

    // Read file content at HEAD for line number computation.
    let content = git.read_file_at_commit(head, file).ok();

    for op in ops {
        let line = render_operation(op, content.as_deref());
        section.push_str(&line);
    }

    section
}

/// Render a single semantic operation as a markdown bullet.
fn render_operation(op: &SemanticOperation, file_content: Option<&[u8]>) -> String {
    match op {
        SemanticOperation::AddSymbol { symbol, .. } => {
            let line = line_info(symbol.byte_range.start, file_content);
            format!(
                "- **Added**: `{}()` ({}{line})\n",
                symbol.name,
                kind_label(symbol.kind),
            )
        }
        SemanticOperation::ModifySymbol { new_entry, .. } => {
            let line = line_info(new_entry.byte_range.start, file_content);
            format!(
                "- **Modified**: `{}()` ({}{line})\n",
                new_entry.name,
                kind_label(new_entry.kind),
            )
        }
        SemanticOperation::DeleteSymbol { id, .. } => {
            // SymbolId is in "scope::name::kind" format — extract the name.
            let parts: Vec<&str> = id.0.split("::").collect();
            let name = if parts.len() >= 2 {
                parts[parts.len() - 2]
            } else {
                &id.0
            };
            format!("- **Deleted**: `{name}()`\n")
        }
        SemanticOperation::AddFile { path } => {
            format!("- **New file**: `{}`\n", path.display())
        }
        SemanticOperation::DeleteFile { path } => {
            format!("- **File deleted**: `{}`\n", path.display())
        }
        SemanticOperation::RawDiff { .. } => {
            "- Raw changes applied (no semantic analysis available)\n".to_string()
        }
    }
}

/// Format a line number suffix like ", line 42".
fn line_info(byte_offset: usize, content: Option<&[u8]>) -> String {
    match content {
        Some(c) => {
            let line = byte_offset_to_line(c, byte_offset);
            format!(", line {line}")
        }
        None => String::new(),
    }
}

/// Convert a byte offset to a 1-indexed line number.
#[allow(clippy::naive_bytecount)]
fn byte_offset_to_line(content: &[u8], offset: usize) -> usize {
    content[..offset.min(content.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// Human-readable label for a `TrunkFileStatus`.
fn status_label(status: &TrunkFileStatus) -> &'static str {
    match status {
        TrunkFileStatus::TrunkVisible => "trunk visible -- you see the new version",
        TrunkFileStatus::Shadowed => "shadowed -- you still see your version",
        TrunkFileStatus::RebaseMerged => "rebased -- merged cleanly",
        TrunkFileStatus::RebaseConflict => "rebased -- CONFLICT",
    }
}

/// Human-readable label for a `SymbolKind`.
fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "Function",
        SymbolKind::Struct => "Struct",
        SymbolKind::Enum => "Enum",
        SymbolKind::Trait => "Trait",
        SymbolKind::Impl => "Impl",
        SymbolKind::Import => "Import",
        SymbolKind::Const => "Const",
        SymbolKind::TypeAlias => "TypeAlias",
        SymbolKind::Module => "Module",
        SymbolKind::Test => "Test",
        SymbolKind::Class => "Class",
        SymbolKind::Interface => "Interface",
        SymbolKind::Method => "Method",
        SymbolKind::Section => "Section",
        SymbolKind::Directive => "Directive",
        SymbolKind::Variable => "Variable",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phantom_core::changeset::SemanticOperation;
    use phantom_core::id::{ContentHash, SymbolId};
    use phantom_core::notification::TrunkFileStatus;
    use phantom_core::symbol::{SymbolEntry, SymbolKind};

    use super::*;

    fn dummy_symbol(
        name: &str,
        kind: SymbolKind,
        byte_start: usize,
        byte_end: usize,
    ) -> SymbolEntry {
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
        assert!(status_label(&TrunkFileStatus::TrunkVisible).contains("trunk visible"));
        assert!(status_label(&TrunkFileStatus::Shadowed).contains("shadowed"));
        assert!(status_label(&TrunkFileStatus::RebaseMerged).contains("merged cleanly"));
        assert!(status_label(&TrunkFileStatus::RebaseConflict).contains("CONFLICT"));
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
        let preamble =
            "# Phantom Agent Session\n\n## Commands\n- submit\n\n---\n\n## Trunk Updates\n";
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
}
