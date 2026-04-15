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

/// Maximum byte size for content passed to `diffy::create_patch`.
/// Myers diff is O(ND); beyond this threshold, diffing can lock the CPU
/// and the LLM cannot process the output anyway.
const MAX_DIFF_BYTE_SIZE: usize = 250_000;

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
   For raw text and dependency conflicts you see OURS and THEIRS as unified
   diffs against BASE (with 3 lines of context). A Scope Context header may
   appear if the file is parseable, listing the enclosing function or type
   signature. Use the Read tool if you need broader file context.
   For binary files you see three labeled blocks.
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

        let mut used_compact = false;

        if is_symbol_conflict {
            used_compact =
                write_compact_conflict(&mut content, lang, conflict, base_short, &parser);
        }

        // Graceful degradation: if AST parsing failed (unsupported language,
        // parse error, missing span), try raw text diff before the expensive
        // three-block fallback.
        if !used_compact && (is_symbol_conflict || is_text_conflict) {
            used_compact =
                write_compact_raw_text_conflict(&mut content, lang, conflict, base_short, &parser);
        }

        if !used_compact {
            // Compute the best truncation center from content divergence so
            // that the token budget window is centered on the actual conflict
            // region rather than naively slicing from byte 0.
            let trunc_center = compute_truncation_center(
                conflict.base_content.as_deref(),
                conflict.ours_content.as_deref(),
                conflict.theirs_content.as_deref(),
            );

            // Fallback: three full code blocks.
            writeln!(content, "#### BASE (common ancestor at {base_short})").unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.base_content.as_deref(),
                conflict.detail.base_span.as_ref(),
                file_path,
                &parser,
                trunc_center,
            );

            writeln!(content, "#### OURS (current trunk)").unwrap();
            write_code_block(
                &mut content,
                lang,
                conflict.ours_content.as_deref(),
                conflict.detail.ours_span.as_ref(),
                file_path,
                &parser,
                trunc_center,
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
                &parser,
                trunc_center,
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

    let filename = match group_index {
        Some(i) => format!(".phantom-task-resolve-{i}.md"),
        None => CONTEXT_FILE.to_string(),
    };
    let path = upper_dir.join(&filename);
    std::fs::write(&path, content)
        .with_context(|| format!("failed to write resolve context file to {}", path.display()))?;

    Ok(path)
}

/// Extract the enclosing symbol's source text and start line from file content.
///
/// Returns `(symbol_text, start_line)` on success, or `None` if the language
/// is unsupported, parsing fails, or no symbol encloses the span.
fn extract_symbol_text(
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
    file_path: &Path,
    parser: &phantom_semantic::Parser,
) -> Option<(String, usize)> {
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
    parser: &phantom_semantic::Parser,
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
        match extract_symbol_text(base_content, base_span, file_path, parser) {
            Some(pair) => pair,
            None => return false,
        };

    // Extract symbol text from OURS and THEIRS (None = side deleted the symbol).
    let ours_symbol = conflict
        .detail
        .ours_span
        .as_ref()
        .and_then(|s| extract_symbol_text(ours_content, s, file_path, parser))
        .map(|(text, _)| text);

    let theirs_symbol = conflict
        .detail
        .theirs_span
        .as_ref()
        .and_then(|s| extract_symbol_text(theirs_content, s, file_path, parser))
        .map(|(text, _)| text);

    // Write BASE once.
    let end_line = base_start_line + base_symbol.lines().count().saturating_sub(1);
    writeln!(out, "#### BASE (common ancestor at {base_short})").unwrap();
    writeln!(out, "Lines {base_start_line}-{end_line}").unwrap();
    writeln!(out, "```{lang}").unwrap();
    writeln!(out, "{base_symbol}").unwrap();
    writeln!(out, "```").unwrap();
    writeln!(out).unwrap();

    // Emit a one-line scope header so the diffs are self-documenting even
    // when the BASE block is large and the signature scrolls out of view.
    let scope_signature = base_symbol.lines().next().unwrap_or("");
    if !scope_signature.is_empty() {
        writeln!(out, "#### Scope Context").unwrap();
        writeln!(out, "`{scope_signature}`").unwrap();
        writeln!(out).unwrap();
    }

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
/// Emits OURS and THEIRS as unified diffs against BASE. The diffs include
/// 3 lines of context around each change, so the full BASE is not shown —
/// the agent can use the Read tool if broader context is needed.
///
/// When the file is parseable by tree-sitter, a **Scope Context** header is
/// emitted listing the enclosing declaration signatures for the changed
/// regions. This prevents the LLM from editing symbols blindly when the
/// 3-line diff context does not reach the function/struct signature.
///
/// Returns `true` if compact format was written.
fn write_compact_raw_text_conflict(
    out: &mut String,
    _lang: &str,
    conflict: &ResolveConflictContext,
    _base_short: &str,
    parser: &phantom_semantic::Parser,
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

    if base_content.len() > MAX_DIFF_BYTE_SIZE
        || ours_content.len() > MAX_DIFF_BYTE_SIZE
        || theirs_content.len() > MAX_DIFF_BYTE_SIZE
    {
        return false;
    }

    // For parseable files, extract enclosing symbol signatures for the
    // changed regions so the LLM knows what scope it is editing.
    let file_path = &conflict.detail.file;
    if let Some(signatures) = collect_scope_signatures(base_content, ours_content, theirs_content, file_path, parser) {
        if !signatures.is_empty() {
            writeln!(out, "#### Scope Context").unwrap();
            for sig in &signatures {
                writeln!(out, "`{sig}`").unwrap();
            }
            writeln!(out).unwrap();
        }
    }

    // Diffs include 3-line context around each change — the full BASE is redundant
    // and would waste tokens. The agent can use the Read tool if broader context is needed.
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

/// Collect unique enclosing-symbol first-line signatures for all diff hunks.
///
/// Returns `None` if the file is not parseable, `Some(vec)` otherwise (possibly
/// empty if no hunks fall inside a known symbol).
fn collect_scope_signatures(
    base_content: &str,
    ours_content: &str,
    theirs_content: &str,
    file_path: &Path,
    parser: &phantom_semantic::Parser,
) -> Option<Vec<String>> {
    if !parser.supports_language(file_path) {
        return None;
    }
    let symbols = parser.parse_file(file_path, base_content.as_bytes()).ok()?;
    if symbols.is_empty() {
        return None;
    }

    let ours_patch = diffy::create_patch(base_content, ours_content);
    let theirs_patch = diffy::create_patch(base_content, theirs_content);

    let mut signatures: Vec<String> = Vec::new();

    for hunk in ours_patch.hunks().iter().chain(theirs_patch.hunks().iter()) {
        let hunk_line = hunk.old_range().start(); // 1-indexed
        let byte_offset = line_to_byte_offset(base_content, hunk_line);
        let target = byte_offset..byte_offset + 1;
        if let Some(sym) = find_enclosing_symbol(&symbols, &target) {
            let sym_text = &base_content[sym.byte_range.start..sym.byte_range.end.min(base_content.len())];
            if let Some(first_line) = sym_text.lines().next() {
                let sig = first_line.to_string();
                if !signatures.contains(&sig) {
                    signatures.push(sig);
                }
            }
        }
    }

    Some(signatures)
}

/// Convert a 1-indexed line number to a byte offset in `content`.
///
/// Returns the byte offset of the first character on the given line,
/// or `content.len()` if the line is beyond the end of the content.
fn line_to_byte_offset(content: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    content
        .as_bytes()
        .iter()
        .enumerate()
        .filter(|&(_, b)| *b == b'\n')
        .nth(line - 2) // line 2 starts after the 1st newline (index 0)
        .map(|(i, _)| i + 1)
        .unwrap_or(content.len())
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

/// Write the enclosing AST node for a conflict span directly to `out`,
/// falling back to ±10 line padding for unsupported languages or when no
/// symbol encloses the span.
fn write_span_context(
    out: &mut String,
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
    file_path: &Path,
    parser: &phantom_semantic::Parser,
) {
    if parser.supports_language(file_path)
        && let Ok(symbols) = parser.parse_file(file_path, content.as_bytes())
        && let Some(enclosing) = find_enclosing_symbol(&symbols, &span.byte_range)
    {
        let start = enclosing.byte_range.start;
        let end = enclosing.byte_range.end.min(content.len());
        out.push_str(&content[start..end]);
        return;
    }
    write_span_lines_fallback(out, content, span);
}

/// Fallback: write lines around a conflict span with ±10 line padding
/// directly to `out`.
fn write_span_lines_fallback(
    out: &mut String,
    content: &str,
    span: &phantom_core::conflict::ConflictSpan,
) {
    let start = span.start_line.saturating_sub(10).max(1) - 1; // zero-indexed
    let count = span.end_line + 10 - start;
    for (i, line) in content.lines().skip(start).take(count).enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
}

/// Find the first byte offset where two strings diverge.
///
/// Returns `None` if the strings are identical.
fn first_divergence_offset(a: &str, b: &str) -> Option<usize> {
    a.as_bytes()
        .iter()
        .zip(b.as_bytes().iter())
        .position(|(x, y)| x != y)
        .or_else(|| {
            if a.len() != b.len() {
                Some(a.len().min(b.len()))
            } else {
                None
            }
        })
}

/// Compute the best center byte for truncation by comparing base against ours/theirs.
///
/// Returns the earliest divergence point, or 0 if no comparison is possible.
fn compute_truncation_center(
    base: Option<&str>,
    ours: Option<&str>,
    theirs: Option<&str>,
) -> usize {
    let mut earliest = usize::MAX;
    if let (Some(b), Some(o)) = (base, ours) {
        if let Some(off) = first_divergence_offset(b, o) {
            earliest = earliest.min(off);
        }
    }
    if let (Some(b), Some(t)) = (base, theirs) {
        if let Some(off) = first_divergence_offset(b, t) {
            earliest = earliest.min(off);
        }
    }
    if earliest == usize::MAX { 0 } else { earliest }
}

/// Write content to `out`, truncating to a window around `center` if needed.
///
/// The window is `WHOLE_FILE_BYTE_BUDGET` bytes centered on `center`, snapped
/// to line boundaries. Truncation markers are emitted when content is cut.
fn write_truncated(out: &mut String, text: &str, center: usize) {
    use std::fmt::Write;

    if text.len() <= WHOLE_FILE_BYTE_BUDGET {
        out.push_str(text);
        return;
    }

    let half = WHOLE_FILE_BYTE_BUDGET / 2;
    let raw_start = center.saturating_sub(half);
    let raw_end = (raw_start + WHOLE_FILE_BYTE_BUDGET).min(text.len());

    // Snap start forward to the first line boundary (unless already at 0).
    let start = if raw_start == 0 {
        0
    } else {
        text[raw_start..]
            .find('\n')
            .map(|i| raw_start + i + 1)
            .unwrap_or(raw_start)
    };

    // Snap end backward to the last line boundary.
    let end = text[..raw_end]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(raw_end);

    // Ensure we still have a non-empty window after snapping.
    let end = end.max(start);

    if start > 0 {
        let lines_above = text[..start].matches('\n').count();
        write!(out, "// ... [CONTENT TRUNCATED: {lines_above} lines above] ...\n").unwrap();
    }

    out.push_str(&text[start..end]);

    if end < text.len() {
        let remaining_tokens = (text.len() - end) / BYTES_PER_TOKEN_ESTIMATE;
        write!(out, "// ... [CONTENT TRUNCATED: ~{remaining_tokens} more tokens below] ...").unwrap();
    }
}

/// Write a fenced code block, trimming to span if available.
/// Streams content directly into `out` without intermediate String allocations.
///
/// When no span is available, truncation is centered on `truncation_center`
/// (the byte offset where the conflict is likely located).
fn write_code_block(
    out: &mut String,
    lang: &str,
    content: Option<&str>,
    span: Option<&phantom_core::conflict::ConflictSpan>,
    file_path: &Path,
    parser: &phantom_semantic::Parser,
    truncation_center: usize,
) {
    use std::fmt::Write;

    match content {
        Some(text) => {
            writeln!(out, "```{lang}").unwrap();
            match span {
                Some(s) => write_span_context(out, text, s, file_path, parser),
                None => write_truncated(out, text, truncation_center),
            }
            out.push('\n');
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
