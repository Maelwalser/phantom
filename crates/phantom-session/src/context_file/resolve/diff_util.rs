//! Shared diff-rendering helpers used by the compact conflict formats.
//!
//! Wraps `diffy` patch generation, stripping its built-in `--- original` /
//! `+++ modified` headers (our markdown already labels each side) while
//! preserving `@@` hunk headers.

use std::fmt::Write;

/// Write a single diff section (OURS or THEIRS) relative to BASE.
pub(super) fn write_diff_section(
    out: &mut String,
    label: &str,
    desc: &str,
    base_symbol: &str,
    modified: Option<&str>,
) {
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

/// Find the 1-indexed old-side line number of the first actual change in a hunk.
///
/// Skips leading context lines (which diffy includes) and returns the line
/// where the first removal or insertion occurs.  Falls back to the hunk's
/// `old_range().start()` if no changed lines are found.
pub(super) fn first_changed_old_line(hunk: &diffy::Hunk<'_, str>) -> usize {
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
pub(super) fn line_to_byte_offset(content: &str, line: usize) -> usize {
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
