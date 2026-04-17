//! Symbol-scoped compact conflict format.
//!
//! Applies to `BothModifiedSymbol` and `ModifyDeleteSymbol` conflicts when all
//! three contents are present, the file is parseable, and a base span is
//! available. Emits BASE once as a full code block, followed by OURS and
//! THEIRS as unified diffs against BASE.

use std::fmt::Write;

use super::super::diff_util::write_diff_section;
use super::super::symbols::extract_symbol_text;
use super::{ConflictFormat, FormatCtx};

pub(super) struct CompactSymbolFormat;

impl ConflictFormat for CompactSymbolFormat {
    fn try_write(&self, out: &mut String, ctx: &FormatCtx<'_>) -> bool {
        let is_symbol_conflict = matches!(
            ctx.conflict.detail.kind,
            phantom_core::ConflictKind::BothModifiedSymbol
                | phantom_core::ConflictKind::ModifyDeleteSymbol
        );
        if !is_symbol_conflict {
            return false;
        }
        write_compact_conflict(out, ctx.lang, ctx.conflict, ctx.base_short, ctx.parser)
    }
}

/// Attempt to write a conflict in compact diff format.
///
/// Shows BASE symbol text once, then OURS and THEIRS as unified diffs
/// against BASE. Returns `true` if the compact format was written,
/// `false` if the caller should fall back to the three-block format.
pub(in crate::context_file::resolve) fn write_compact_conflict(
    out: &mut String,
    lang: &str,
    conflict: &super::super::ResolveConflictContext,
    base_short: &str,
    parser: &phantom_semantic::Parser,
) -> bool {
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
