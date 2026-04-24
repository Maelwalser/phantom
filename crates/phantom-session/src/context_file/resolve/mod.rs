//! Conflict-resolution context file generation.
//!
//! Generates `.phantom-task.md` files with three-version diffs for background
//! AI agents to resolve merge conflicts.
//!
//! The conflict rendering is split across a small pipeline:
//! - [`symbols`] — enclosing-symbol extraction via tree-sitter
//! - [`diff_util`] — `diffy`-based patch rendering
//! - [`truncate`] — windowed truncation around divergence points
//! - [`formats`] — [`formats::ConflictFormat`] implementations tried in order
//!
//! Adding a new presentation: see `formats/mod.rs`.

mod diff_util;
mod formats;
mod kind;
mod symbols;
mod truncate;

#[cfg(test)]
mod tests;

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};

use super::{CONTEXT_FILE, lang_from_path};
use formats::{FormatCtx, dispatch};
use kind::format_conflict_kind;

/// Context for a single conflict, with the three-way file content.
pub struct ResolveConflictContext {
    /// The conflict detail from the event log.
    pub detail: phantom_core::ConflictDetail,
    /// Content of the file at the changeset's base commit (common ancestor).
    pub base_content: Option<String>,
    /// Content of the file at the current trunk HEAD.
    pub ours_content: Option<String>,
    /// Content of the file in the agent's overlay (upper layer).
    pub theirs_content: Option<String>,
}

/// Write the static resolution rules to a file for system prompt injection.
///
/// This file is passed via `--append-system-prompt-file` so its content becomes
/// part of the cached system prompt prefix. Because the content is 100% static,
/// it maximises prompt cache hit rates across all conflict resolution sessions.
pub fn write_resolve_rules_file(path: &Path) -> anyhow::Result<()> {
    let content = "\
# Phantom Conflict Resolution Rules

## Resolution Rules

1. For symbol-level conflicts you are shown a Scope Context line identifying
   the enclosing declaration, then the BASE version as full source, then OURS
   and THEIRS as unified diffs showing what each side changed relative to BASE.
   Your working directory has the THEIRS version — integrate the OURS changes.
   Do not re-read the file; use the content shown here.
   For raw text and dependency conflicts in parseable files, diffs are scoped
   to the enclosing symbol boundary with a Scope Context header identifying
   each declaration. For non-parseable files, OURS and THEIRS are shown as
   unified diffs against the full BASE (with 3 lines of context).
   Use the Read tool if you need broader file context.
   For other conflicts (binary files, oversized files, or missing content) you
   see a single block with diff3-style conflict markers (`<<<<<<< ours`,
   `||||||| original`, `=======`, `>>>>>>> theirs`). Resolve the markers
   in place.
2. Your goal: produce a merged version that preserves the intent of BOTH sides.
3. NEVER silently drop code from either side unless one side explicitly deleted it.
4. For BothModifiedSymbol conflicts: merge both sets of changes into the symbol.
   If they modify different parts, combine them. If they make contradictory
   changes to the same lines, prefer the more complete version and leave a
   comment explaining the choice.
5. For ModifyDeleteSymbol conflicts: keep the modification unless the deletion
   was clearly intentional (e.g., functionality moved elsewhere).
6. For dependency version conflicts: pick the higher version unless there is a
   compatibility constraint.
7. Edit ONLY the files listed in the conflict context file. Do not modify unrelated files.
8. After editing, verify the file still parses correctly.
9. If you cannot resolve a conflict with confidence, leave a marker
   `PHANTOM_UNRESOLVED: <reason>` using the file's native comment syntax
   (e.g. `// …` in Rust/TS/Go, `# …` in Python/YAML/Shell).
   If the file format does not support comments (e.g. JSON), insert a
   root-level key instead: `\"PHANTOM_UNRESOLVED\": \"<reason>\"`.

## After Resolution
Your changes will be automatically submitted and materialized when you finish.
";

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }

    std::fs::write(path, content)
        .with_context(|| format!("failed to write resolve rules to {}", path.display()))?;

    Ok(())
}

/// Write a conflict-resolution context file into the overlay.
///
/// Generates a `.phantom-task.md` (or `.phantom-task-resolve-{i}.md` when
/// `group_index` is `Some(i)`) with three-version diffs for a background
/// Claude Code agent. Static resolution rules are injected separately via
/// `--append-system-prompt-file` (see [`write_resolve_rules_file`]).
///
/// Returns the path of the written context file.
pub fn write_resolve_context_file(
    upper_dir: &Path,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    conflicts: &[ResolveConflictContext],
    group_index: Option<usize>,
) -> anyhow::Result<std::path::PathBuf> {
    use std::fmt::Write;

    let base_hex = base_commit.to_hex();
    let base_short = &base_hex[..12.min(base_hex.len())];

    let parser = phantom_semantic::Parser::new();
    let mut content = String::new();
    let _ = writeln!(content, "# Phantom Conflict Resolution");
    let _ = writeln!(content);
    let _ = writeln!(
        content,
        "You are resolving merge conflicts in a Phantom overlay. Your changes are"
    );
    let _ = writeln!(content, "isolated from trunk and other agents.");
    let _ = writeln!(content);
    let _ = writeln!(content, "## Agent Info");
    let _ = writeln!(content, "- Agent: {agent_id}");
    let _ = writeln!(content, "- Changeset: {changeset_id}");
    let _ = writeln!(content, "- Base commit: {base_short}");
    let _ = writeln!(content);
    let _ = writeln!(content, "## Conflicts");

    for (i, conflict) in conflicts.iter().enumerate() {
        let _ = writeln!(content);
        let kind_label = format_conflict_kind(conflict.detail.kind);
        let _ = writeln!(
            content,
            "### Conflict {}: {} [{}]",
            i + 1,
            conflict.detail.file.display(),
            kind_label
        );
        let _ = writeln!(content, "{}", conflict.detail.description);
        let _ = writeln!(content);

        let lang = lang_from_path(&conflict.detail.file);
        let ctx = FormatCtx {
            lang,
            conflict,
            base_short,
            parser: &parser,
        };
        dispatch(&mut content, &ctx);

        let _ = writeln!(
            content,
            "Edit this file in your working directory to merge both changes."
        );
        let _ = writeln!(content);
        let _ = writeln!(content, "---");
    }

    let filename = match group_index {
        Some(i) => format!(".phantom-task-resolve-{i}.md"),
        None => CONTEXT_FILE.to_string(),
    };
    let path = upper_dir.join(&filename);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write resolve context file to {}", path.display()))?;

    Ok(path)
}
