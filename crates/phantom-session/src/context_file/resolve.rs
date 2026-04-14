//! Conflict-resolution context file generation.
//!
//! Generates `.phantom-task.md` files with three-version diffs for background
//! AI agents to resolve merge conflicts.

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::symbol::find_enclosing_symbol;

use super::{lang_from_path, CONTEXT_FILE};

/// Approximate byte budget for whole-file display in conflict context.
/// ~8192 tokens × 4 bytes/token = 32,768 bytes.
const WHOLE_FILE_BYTE_BUDGET: usize = 32_768;
const BYTES_PER_TOKEN_ESTIMATE: usize = 4;

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

1. For symbol-level conflicts you are shown the BASE version as full source,
   then OURS and THEIRS as unified diffs showing what each side changed
   relative to BASE. Your working directory has the THEIRS version —
   integrate the OURS changes. Do not re-read the file; use the content shown here.
   For raw text and dependency conflicts you see BASE as full text, then
   OURS and THEIRS as unified diffs. For binary files you see three labeled blocks.
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
9. If you cannot resolve a conflict with confidence, leave a comment
   using the file's native comment syntax containing the marker
   `PHANTOM_UNRESOLVED: <reason>` (e.g. `// …` in Rust/TS/Go,
   `# …` in Python/YAML/Shell).

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
/// Generates a `.phantom-task.md` with three-version diffs for a background
/// Claude Code agent. Static resolution rules are injected separately via
/// `--append-system-prompt-file` (see [`write_resolve_rules_file`]).
pub fn write_resolve_context_file(
    upper_dir: &Path,
    agent_id: &AgentId,
    changeset_id: &ChangesetId,
    base_commit: &GitOid,
    conflicts: &[ResolveConflictContext],
) -> anyhow::Result<()> {
    use std::fmt::Write;

    let base_hex = base_commit.to_hex();
    let base_short = &base_hex[..12.min(base_hex.len())];

    let mut content = String::new();
    writeln!(content, "# Phantom Conflict Resolution").unwrap();
    writeln!(content).unwrap();
    writeln!(
        content,
        "You are resolving merge conflicts in a Phantom overlay. Your changes are"
    )
    .unwrap();
    writeln!(content, "isolated from trunk and other agents.").unwrap();
    writeln!(content).unwrap();
    writeln!(content, "## Agent Info").unwrap();
    writeln!(content, "- Agent: {agent_id}").unwrap();
    writeln!(content, "- Changeset: {changeset_id}").unwrap();
    writeln!(content, "- Base commit: {base_short}").unwrap();
    writeln!(content).unwrap();
    writeln!(content, "## Conflicts").unwrap();

    for (i, conflict) in conflicts.iter().enumerate() {
        writeln!(content).unwrap();
        let kind_label = format_conflict_kind(conflict.detail.kind);
        writeln!(
            content,
            "### Conflict {}: {} [{}]",
            i + 1,
            conflict.detail.file.display(),
            kind_label
        )
        .unwrap();
        writeln!(content, "{}", conflict.detail.description).unwrap();
        writeln!(content).unwrap();

        let lang = lang_from_path(&conflict.detail.file);
        let file_path = &conflict.detail.file;

        // Try compact diff format for symbol-level conflicts.
        let is_symbol_conflict = matches!(
            conflict.detail.kind,
            phantom_core::ConflictKind::BothModifiedSymbol
                | phantom_core::ConflictKind::ModifyDeleteSymbol
        );

        let is_text_conflict = matches!(
            conflict.detail.kind,
            phantom_core::ConflictKind::RawTextConflict
                | phantom_core::ConflictKind::BothModifiedDependencyVersion
        );

        let used_compact = if is_symbol_conflict {
            write_compact_conflict(&mut content, lang, conflict, base_short)
        } else if is_text_conflict {
            write_compact_raw_text_conflict(&mut content, lang, conflict, base_short)
        } else {
            false
        };

        if !used_compact {
            // Fallback: three full code blocks.
            writeln!(content, "#### BASE (common ancestor at {base_short})").unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.base_content.as_deref(),
                conflict.detail.base_span.as_ref(),
                file_path,
            );

            writeln!(content, "#### OURS (current trunk)").unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.ours_content.as_deref(),
                conflict.detail.ours_span.as_ref(),
                file_path,
            );

            writeln!(
                content,
                "#### THEIRS (agent's version \u{2014} this is the current state of the file in your working directory; do not re-read it)"
            )
            .unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.theirs_content.as_deref(),
                conflict.detail.theirs_span.as_ref(),
                file_path,
            );
        }

        writeln!(
            content,
            "Edit this file in your working directory to merge both changes."
        )
        .unwrap();
        writeln!(content).unwrap();
        writeln!(content, "---").unwrap();
    }

    let path = upper_dir.join(CONTEXT_FILE);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write resolve context file to {}", path.display()))?;

    Ok(())
}

/// Extract the enclosing symbol's source text and start line from file content.
///
/// Returns `(symbol_text, start_line)` on success, or `None` if the language
/// is unsupported, parsing fails, or no symbol encloses the span.
fn extract_symbol_text(
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
    file_path: &Path,
) -> Option<(String, usize)> {
    let parser = phantom_semantic::Parser::new();
    if !parser.supports_language(file_path) {
        return None;
    }
    let symbols = parser.parse_file(file_path, content.as_bytes()).ok()?;
    let enclosing = find_enclosing_symbol(&symbols, &span.byte_range)?;
    let start = enclosing.byte_range.start;
    let end = enclosing.byte_range.end.min(content.len());
    let text = content[start..end].to_string();
    let start_line = content[..start].matches('\n').count() + 1;
    Some((text, start_line))
}

/// Attempt to write a conflict in compact diff format.
///
/// Shows BASE symbol text once, then OURS and THEIRS as unified diffs
/// against BASE. Returns `true` if the compact format was written,
/// `false` if the caller should fall back to the three-block format.
fn write_compact_conflict(
    out: &mut String,
    lang: &str,
    conflict: &ResolveConflictContext,
    base_short: &str,
) -> bool {
    use std::fmt::Write;

    let file_path = &conflict.detail.file;

    // Require all three contents and a base span.
    let (base_content, ours_content, theirs_content) = match (
        conflict.base_content.as_deref(),
        conflict.ours_content.as_deref(),
        conflict.theirs_content.as_deref(),
    ) {
        (Some(b), Some(o), Some(t)) => (b, o, t),
        _ => return false,
    };

    let base_span = match conflict.detail.base_span.as_ref() {
        Some(s) => s,
        None => return false,
    };

    // Extract symbol text from BASE.
    let (base_symbol, base_start_line) =
        match extract_symbol_text(base_content, base_span, file_path) {
            Some(pair) => pair,
            None => return false,
        };

    // Extract symbol text from OURS and THEIRS (None = side deleted the symbol).
    let ours_symbol = conflict
        .detail
        .ours_span
        .as_ref()
        .and_then(|s| extract_symbol_text(ours_content, s, file_path))
        .map(|(text, _)| text);

    let theirs_symbol = conflict
        .detail
        .theirs_span
        .as_ref()
        .and_then(|s| extract_symbol_text(theirs_content, s, file_path))
        .map(|(text, _)| text);

    // Write BASE once.
    let end_line = base_start_line + base_symbol.lines().count().saturating_sub(1);
    writeln!(out, "#### BASE (common ancestor at {base_short})").unwrap();
    writeln!(out, "Lines {base_start_line}-{end_line}").unwrap();
    writeln!(out, "```{lang}").unwrap();
    writeln!(out, "{base_symbol}").unwrap();
    writeln!(out, "```").unwrap();
    writeln!(out).unwrap();

    // Write OURS diff.
    write_diff_section(out, "OURS", "trunk applied these changes", &base_symbol, ours_symbol.as_deref());

    // Write THEIRS diff.
    write_diff_section(
        out,
        "THEIRS",
        "agent applied these changes \u{2014} this is what is in your working directory; do not re-read the file",
        &base_symbol,
        theirs_symbol.as_deref(),
    );

    true
}

/// Attempt to write a conflict in compact diff format for raw text conflicts.
///
/// Shows BASE content once (truncated to token budget), then OURS and THEIRS
/// as unified diffs against BASE. Works for any text file regardless of
/// tree-sitter support. Returns `true` if compact format was written.
fn write_compact_raw_text_conflict(
    out: &mut String,
    lang: &str,
    conflict: &ResolveConflictContext,
    base_short: &str,
) -> bool {
    use std::fmt::Write;

    let (base_content, ours_content, theirs_content) = match (
        conflict.base_content.as_deref(),
        conflict.ours_content.as_deref(),
        conflict.theirs_content.as_deref(),
    ) {
        (Some(b), Some(o), Some(t)) => (b, o, t),
        _ => return false,
    };

    let base_display = truncate_to_token_budget(base_content);

    writeln!(out, "#### BASE (common ancestor at {base_short})").unwrap();
    writeln!(out, "```{lang}").unwrap();
    writeln!(out, "{base_display}").unwrap();
    writeln!(out, "```").unwrap();
    writeln!(out).unwrap();

    // Write OURS and THEIRS as diffs against the full base (not the truncated version).
    write_diff_section(
        out,
        "OURS",
        "trunk applied these changes",
        base_content,
        Some(ours_content),
    );

    write_diff_section(
        out,
        "THEIRS",
        "agent applied these changes \u{2014} this is what is in your working directory; do not re-read the file",
        base_content,
        Some(theirs_content),
    );

    true
}

/// Write a single diff section (OURS or THEIRS) relative to BASE.
fn write_diff_section(
    out: &mut String,
    label: &str,
    desc: &str,
    base_symbol: &str,
    modified: Option<&str>,
) {
    use std::fmt::Write;

    writeln!(out, "#### {label} ({desc})").unwrap();
    match modified {
        Some(text) if text == base_symbol => {
            writeln!(out, "*(identical to BASE)*").unwrap();
        }
        Some(text) => {
            let patch = diffy::create_patch(base_symbol, text);
            let patch_str = patch.to_string();
            // Skip the `--- original` and `+++ modified` header lines —
            // the surrounding markdown already labels each side.
            // diffy always emits exactly two header lines, so skip(2) is correct.
            writeln!(out, "```diff").unwrap();
            for line in patch_str.lines().skip(2) {
                writeln!(out, "{line}").unwrap();
            }
            writeln!(out, "```").unwrap();
        }
        None => {
            writeln!(out, "*(symbol deleted)*").unwrap();
        }
    }
    writeln!(out).unwrap();
}

/// Extract the enclosing AST node for a conflict span, falling back to ±10
/// line padding for unsupported languages or when no symbol encloses the span.
fn extract_span_context(
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
    file_path: &Path,
) -> String {
    let parser = phantom_semantic::Parser::new();
    if parser.supports_language(file_path)
        && let Ok(symbols) = parser.parse_file(file_path, content.as_bytes())
        && let Some(enclosing) = find_enclosing_symbol(&symbols, &span.byte_range)
    {
        let start = enclosing.byte_range.start;
        let end = enclosing.byte_range.end.min(content.len());
        return content[start..end].to_string();
    }
    extract_span_lines_fallback(content, span)
}

/// Fallback: extract lines around a conflict span with ±10 line padding.
fn extract_span_lines_fallback(
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = span.start_line.saturating_sub(10).max(1) - 1; // zero-indexed
    let end = (span.end_line + 10).min(lines.len());
    lines[start..end].join("\n")
}

/// Truncate content to fit within the token budget, breaking at the last
/// complete line before the byte limit.
fn truncate_to_token_budget(text: &str) -> String {
    if text.len() <= WHOLE_FILE_BYTE_BUDGET {
        return text.to_string();
    }
    let cut = text[..WHOLE_FILE_BYTE_BUDGET]
        .rfind('\n')
        .unwrap_or(WHOLE_FILE_BYTE_BUDGET);
    let remaining_tokens = (text.len() - cut) / BYTES_PER_TOKEN_ESTIMATE;
    format!(
        "{}\n// ... truncated (~{remaining_tokens} more tokens)",
        &text[..cut]
    )
}

/// Write a fenced code block, trimming to span if available.
fn write_code_block(
    out: &mut String,
    lang: &str,
    content: Option<&str>,
    span: Option<&phantom_core::conflict::ConflictSpan>,
    file_path: &Path,
) {
    use std::fmt::Write;

    match content {
        Some(text) => {
            let display = match span {
                Some(s) => extract_span_context(text, s, file_path),
                None => truncate_to_token_budget(text),
            };
            writeln!(out, "```{lang}").unwrap();
            writeln!(out, "{display}").unwrap();
            writeln!(out, "```").unwrap();
        }
        None => {
            writeln!(out, "*(file not found at this version)*").unwrap();
        }
    }
    writeln!(out).unwrap();
}

/// Map a conflict kind to a human-readable label.
fn format_conflict_kind(kind: phantom_core::ConflictKind) -> &'static str {
    match kind {
        phantom_core::ConflictKind::BothModifiedSymbol => "BothModifiedSymbol",
        phantom_core::ConflictKind::ModifyDeleteSymbol => "ModifyDeleteSymbol",
        phantom_core::ConflictKind::BothModifiedDependencyVersion => {
            "BothModifiedDependencyVersion"
        }
        phantom_core::ConflictKind::RawTextConflict => "RawTextConflict",
        phantom_core::ConflictKind::BinaryFile => "BinaryFile",
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
