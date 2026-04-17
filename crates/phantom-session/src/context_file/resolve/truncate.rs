//! Content truncation for conflict context files.
//!
//! Most file-level conflict views are windowed around the first divergence
//! point between the three versions so that, when a file is larger than the
//! token budget, the LLM sees the conflict region rather than the prologue.

use std::fmt::Write;

/// Approximate byte budget for whole-file display in conflict context.
/// ~8192 tokens × 4 bytes/token = 32,768 bytes.
const WHOLE_FILE_BYTE_BUDGET: usize = 32_768;
const BYTES_PER_TOKEN_ESTIMATE: usize = 4;

/// Find the first byte offset where two strings diverge.
///
/// Returns `None` if the strings are identical.
pub(super) fn first_divergence_offset(a: &str, b: &str) -> Option<usize> {
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
pub(super) fn compute_truncation_center(
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
pub(super) fn write_truncated(out: &mut String, text: &str, center: usize) {
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
