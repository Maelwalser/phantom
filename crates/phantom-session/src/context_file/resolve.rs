//! Conflict-resolution context file generation.
//!
//! Generates `.phantom-task.md` files with three-version diffs for background
//! AI agents to resolve merge conflicts.

use std::path::Path;

use anyhow::Context;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::symbol::find_enclosing_symbol;

use super::{CONTEXT_FILE, lang_from_path};

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

            // Guard: don't run diffy merge on oversized content (Myers is O(ND)).
            let any_oversized = [
                conflict.base_content.as_deref(),
                conflict.ours_content.as_deref(),
                conflict.theirs_content.as_deref(),
            ]
            .iter()
            .any(|c| c.is_some_and(|s| s.len() > MAX_DIFF_BYTE_SIZE));

            let merged = if any_oversized {
                None
            } else {
                build_conflict_marker_view(
                    conflict.base_content.as_deref(),
                    conflict.ours_content.as_deref(),
                    conflict.theirs_content.as_deref(),
                )
            };

            if let Some(ref merged_text) = merged {
                writeln!(content, "#### Conflict (diff3 markers \u{2014} OURS = trunk, THEIRS = agent's working directory)").unwrap();
                writeln!(content, "```{lang}").unwrap();
                write_truncated(&mut content, merged_text, trunc_center);
                content.push('\n');
                writeln!(content, "```").unwrap();
                writeln!(content).unwrap();
            } else {
                write_minimal_fallback(
                    &mut content,
                    lang,
                    conflict.base_content.as_deref(),
                    conflict.ours_content.as_deref(),
                    conflict.theirs_content.as_deref(),
                    base_short,
                    trunc_center,
                );
            }
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
    let (Some(base_content), Some(ours_content), Some(theirs_content)) = (
        conflict.base_content.as_deref(),
        conflict.ours_content.as_deref(),
        conflict.theirs_content.as_deref(),
    ) else {
        return false;
    };

    let Some(base_span) = conflict.detail.base_span.as_ref() else {
        return false;
    };

    // Extract symbol text from BASE.
    let Some((base_symbol, base_start_line)) =
        extract_symbol_text(base_content, base_span, file_path, parser)
    else {
        return false;
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
    write_diff_section(
        out,
        "OURS",
        "trunk applied these changes",
        &base_symbol,
        ours_symbol.as_deref(),
    );

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

    let (Some(base_content), Some(ours_content), Some(theirs_content)) = (
        conflict.base_content.as_deref(),
        conflict.ours_content.as_deref(),
        conflict.theirs_content.as_deref(),
    ) else {
        return false;
    };

    if base_content.len() > MAX_DIFF_BYTE_SIZE
        || ours_content.len() > MAX_DIFF_BYTE_SIZE
        || theirs_content.len() > MAX_DIFF_BYTE_SIZE
    {
        return false;
    }

    // For parseable files, restrict diffs to symbol byte ranges instead of
    // diffing the entire file.  This eliminates irrelevant context lines and
    // focuses the LLM on the actual conflict region.
    let file_path = &conflict.detail.file;
    if let Some(scope_symbols) = collect_scope_symbols(
        base_content,
        ours_content,
        theirs_content,
        file_path,
        parser,
    ) && !scope_symbols.is_empty()
    {
        let checkpoint = out.len();
        let mut all_scoped = true;

        for sym in &scope_symbols {
            let base_slice =
                &base_content[sym.base_range.start..sym.base_range.end.min(base_content.len())];

            let ours_range = find_matching_symbol_range(
                ours_content,
                &sym.base_range,
                base_content,
                file_path,
                parser,
            );
            let theirs_range = find_matching_symbol_range(
                theirs_content,
                &sym.base_range,
                base_content,
                file_path,
                parser,
            );

            if ours_range.is_none() || theirs_range.is_none() {
                all_scoped = false;
                break;
            }

            let ours_range = ours_range.unwrap();
            let theirs_range = theirs_range.unwrap();
            let ours_slice = &ours_content[ours_range.start..ours_range.end];
            let theirs_slice = &theirs_content[theirs_range.start..theirs_range.end];

            writeln!(out, "#### Scope Context").unwrap();
            writeln!(out, "`{}`", sym.signature).unwrap();
            writeln!(out).unwrap();

            write_diff_section(
                out,
                "OURS",
                "trunk applied these changes",
                base_slice,
                Some(ours_slice),
            );

            write_diff_section(
                out,
                "THEIRS",
                "agent applied these changes \u{2014} this is what is in your working directory; do not re-read the file",
                base_slice,
                Some(theirs_slice),
            );
        }

        if all_scoped {
            return true;
        }
        // Scoping failed partway — truncate partial output and fall
        // through to whole-file diff.
        out.truncate(checkpoint);
    }

    // Whole-file diff fallback: 3-line context around each change.
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

/// A symbol scope extracted from BASE, used to restrict diffs to symbol
/// boundaries instead of diffing the entire file.
struct ScopeSymbol {
    /// First line of the symbol source (used as a scope header).
    signature: String,
    /// Byte range of the symbol in BASE content.
    base_range: std::ops::Range<usize>,
}

/// Collect unique enclosing symbols for all diff hunks in BASE.
///
/// Returns `None` if the file is not parseable, `Some(vec)` otherwise (possibly
/// empty if no hunks fall inside a known symbol).
fn collect_scope_symbols(
    base_content: &str,
    ours_content: &str,
    theirs_content: &str,
    file_path: &Path,
    parser: &phantom_semantic::Parser,
) -> Option<Vec<ScopeSymbol>> {
    if !parser.supports_language(file_path) {
        return None;
    }
    let symbols = parser.parse_file(file_path, base_content.as_bytes()).ok()?;
    if symbols.is_empty() {
        return None;
    }

    let ours_patch = diffy::create_patch(base_content, ours_content);
    let theirs_patch = diffy::create_patch(base_content, theirs_content);

    let mut scope_symbols: Vec<ScopeSymbol> = Vec::new();

    for hunk in ours_patch.hunks().iter().chain(theirs_patch.hunks().iter()) {
        // Find the first actually changed line in the hunk (skip context lines).
        // diffy's old_range().start() includes leading context which may point
        // into an unrelated symbol.
        let hunk_line = first_changed_old_line(hunk);
        let byte_offset = line_to_byte_offset(base_content, hunk_line);
        let target = byte_offset..byte_offset + 1;
        if let Some(sym) = find_enclosing_symbol(&symbols, &target) {
            // Deduplicate by byte range (more robust than string comparison).
            if scope_symbols.iter().any(|s| s.base_range == sym.byte_range) {
                continue;
            }
            let end = sym.byte_range.end.min(base_content.len());
            let sym_text = &base_content[sym.byte_range.start..end];
            if let Some(first_line) = sym_text.lines().next() {
                scope_symbols.push(ScopeSymbol {
                    signature: first_line.to_string(),
                    base_range: sym.byte_range.clone(),
                });
            }
        }
    }

    Some(scope_symbols)
}

/// Find the matching symbol in `content` by parsing it and locating the symbol
/// that encloses the same probe point (mapped from BASE line to target line via
/// hunk offset).  Falls back to returning `None` if parsing fails or no
/// enclosing symbol is found.
fn find_matching_symbol_range(
    content: &str,
    base_range: &std::ops::Range<usize>,
    base_content: &str,
    file_path: &Path,
    parser: &phantom_semantic::Parser,
) -> Option<std::ops::Range<usize>> {
    let symbols = parser.parse_file(file_path, content.as_bytes()).ok()?;
    // Use the midpoint of the BASE range to find the corresponding line, then
    // probe the same line in the target content.  This is approximate but works
    // well when the symbol hasn't been drastically moved.
    let base_mid = usize::midpoint(base_range.start, base_range.end);
    let base_line = base_content[..base_mid.min(base_content.len())]
        .matches('\n')
        .count()
        + 1;
    let probe_offset = line_to_byte_offset(content, base_line);
    let probe = probe_offset..probe_offset + 1;
    let enclosing = find_enclosing_symbol(&symbols, &probe)?;
    Some(enclosing.byte_range.start..enclosing.byte_range.end.min(content.len()))
}

/// Find the 1-indexed old-side line number of the first actual change in a hunk.
///
/// Skips leading context lines (which diffy includes) and returns the line
/// where the first removal or insertion occurs.  Falls back to the hunk's
/// `old_range().start()` if no changed lines are found.
fn first_changed_old_line(hunk: &diffy::Hunk<'_, str>) -> usize {
    let mut line = hunk.old_range().start(); // 1-indexed
    for diff_line in hunk.lines() {
        match diff_line {
            diffy::Line::Context(_) => line += 1,
            diffy::Line::Delete(_) | diffy::Line::Insert(_) => return line,
        }
    }
    hunk.old_range().start()
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
        .map_or(content.len(), |(i, _)| i + 1)
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

/// Produce a single string with diff3-style conflict markers embedded at
/// divergence points.
///
/// Returns `None` if any of the three versions is missing (the caller should
/// use `write_minimal_fallback` instead).
fn build_conflict_marker_view(
    base: Option<&str>,
    ours: Option<&str>,
    theirs: Option<&str>,
) -> Option<String> {
    let (Some(b), Some(o), Some(t)) = (base, ours, theirs) else {
        return None;
    };
    // diffy::MergeOptions defaults to ConflictStyle::Diff3 which emits
    // <<<<<<< ours / ||||||| original / ======= / >>>>>>> theirs markers.
    let merged = match diffy::MergeOptions::new().merge(b, o, t) {
        Ok(clean) => clean,
        Err(conflicted) => conflicted,
    };
    Some(merged)
}

/// Last-resort fallback when conflict marker generation is not possible
/// (missing content or oversized files). Emits whatever content is available
/// as a single labeled, truncated code block.
fn write_minimal_fallback(
    out: &mut String,
    lang: &str,
    base: Option<&str>,
    ours: Option<&str>,
    theirs: Option<&str>,
    base_short: &str,
    trunc_center: usize,
) {
    use std::fmt::Write;

    // Pick the best available content: prefer theirs (agent's working copy),
    // then ours (trunk), then base.
    let (label, text) = if let Some(t) = theirs {
        ("THEIRS (agent's working directory)".to_string(), t)
    } else if let Some(o) = ours {
        ("OURS (current trunk)".to_string(), o)
    } else if let Some(b) = base {
        (format!("BASE (common ancestor at {base_short})"), b)
    } else {
        writeln!(out, "*(no file content available for any version)*").unwrap();
        writeln!(out).unwrap();
        return;
    };

    writeln!(out, "#### {label}").unwrap();
    writeln!(out, "```{lang}").unwrap();
    write_truncated(out, text, trunc_center);
    out.push('\n');
    writeln!(out, "```").unwrap();
    writeln!(out).unwrap();
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
            if a.len() == b.len() {
                None
            } else {
                Some(a.len().min(b.len()))
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
    if let (Some(b), Some(o)) = (base, ours)
        && let Some(off) = first_divergence_offset(b, o)
    {
        earliest = earliest.min(off);
    }
    if let (Some(b), Some(t)) = (base, theirs)
        && let Some(off) = first_divergence_offset(b, t)
    {
        earliest = earliest.min(off);
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
            .map_or(raw_start, |i| raw_start + i + 1)
    };

    // Snap end backward to the last line boundary.
    let end = text[..raw_end].rfind('\n').map_or(raw_end, |i| i + 1);

    // Ensure we still have a non-empty window after snapping.
    let end = end.max(start);

    if start > 0 {
        let lines_above = text[..start].matches('\n').count();
        writeln!(
            out,
            "// ... [CONTENT TRUNCATED: {lines_above} lines above] ..."
        )
        .unwrap();
    }

    out.push_str(&text[start..end]);

    if end < text.len() {
        let remaining_tokens = (text.len() - end) / BYTES_PER_TOKEN_ESTIMATE;
        write!(
            out,
            "// ... [CONTENT TRUNCATED: ~{remaining_tokens} more tokens below] ..."
        )
        .unwrap();
    }
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
mod tests {
    use super::*;

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
        let ok =
            write_compact_raw_text_conflict(&mut out, "markdown", &conflict, "abc123", &parser);

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
}
