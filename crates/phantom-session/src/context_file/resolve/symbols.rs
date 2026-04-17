//! Symbol extraction helpers for conflict resolution.
//!
//! Wraps tree-sitter parsing to map conflict byte spans back to the
//! enclosing symbol (function, struct, etc.), so conflict diffs can be
//! scoped to semantic boundaries instead of arbitrary line windows.

use std::path::Path;

use phantom_core::symbol::find_enclosing_symbol;

use super::diff_util::{first_changed_old_line, line_to_byte_offset};

/// A symbol scope extracted from BASE, used to restrict diffs to symbol
/// boundaries instead of diffing the entire file.
pub(super) struct ScopeSymbol {
    /// First line of the symbol source (used as a scope header).
    pub(super) signature: String,
    /// Byte range of the symbol in BASE content.
    pub(super) base_range: std::ops::Range<usize>,
}

/// Extract the enclosing symbol's source text and start line from file content.
///
/// Returns `(symbol_text, start_line)` on success, or `None` if the language
/// is unsupported, parsing fails, or no symbol encloses the span.
pub(super) fn extract_symbol_text(
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

/// Collect unique enclosing symbols for all diff hunks in BASE.
///
/// Returns `None` if the file is not parseable, `Some(vec)` otherwise (possibly
/// empty if no hunks fall inside a known symbol).
pub(super) fn collect_scope_symbols(
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
pub(super) fn find_matching_symbol_range(
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
