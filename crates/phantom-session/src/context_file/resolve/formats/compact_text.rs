//! File-scoped compact conflict format.
//!
//! Used for `RawTextConflict` and `BothModifiedDependencyVersion`, and as the
//! fallback from `CompactSymbolFormat` when AST parsing fails for a symbol
//! conflict. Emits OURS and THEIRS as unified diffs against BASE; for
//! parseable files, the diffs are scoped to enclosing symbol boundaries with
//! per-symbol `Scope Context` headers.

use std::fmt::Write;

use super::super::diff_util::write_diff_section;
use super::super::symbols::{collect_scope_symbols, find_matching_symbol_range};
use super::{ConflictFormat, FormatCtx, MAX_DIFF_BYTE_SIZE};

pub(super) struct CompactTextFormat;

impl ConflictFormat for CompactTextFormat {
    fn try_write(&self, out: &mut String, ctx: &FormatCtx<'_>) -> bool {
        let kind = ctx.conflict.detail.kind;
        let is_symbol_conflict = matches!(
            kind,
            phantom_core::ConflictKind::BothModifiedSymbol
                | phantom_core::ConflictKind::ModifyDeleteSymbol
        );
        let is_text_conflict = matches!(
            kind,
            phantom_core::ConflictKind::RawTextConflict
                | phantom_core::ConflictKind::BothModifiedDependencyVersion
        );
        if !(is_symbol_conflict || is_text_conflict) {
            return false;
        }
        write_compact_raw_text_conflict(out, ctx.lang, ctx.conflict, ctx.base_short, ctx.parser)
    }
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
pub(in crate::context_file::resolve) fn write_compact_raw_text_conflict(
    out: &mut String,
    _lang: &str,
    conflict: &super::super::ResolveConflictContext,
    _base_short: &str,
    parser: &phantom_semantic::Parser,
) -> bool {
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
