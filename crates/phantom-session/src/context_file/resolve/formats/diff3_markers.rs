//! Diff3-style conflict marker format.
//!
//! Emits a single code block with `diffy`'s three-way merge output — clean
//! regions where the merge succeeded, bracketed `<<<<<<< ours / ||||||| original
//! / ======= / >>>>>>> theirs` blocks where it could not. Used when all three
//! versions are present and none exceeds the diff size budget.

use std::fmt::Write;

use super::super::truncate::{compute_truncation_center, write_truncated};
use super::{ConflictFormat, FormatCtx, MAX_DIFF_BYTE_SIZE};

pub(super) struct Diff3MarkersFormat;

impl ConflictFormat for Diff3MarkersFormat {
    fn try_write(&self, out: &mut String, ctx: &FormatCtx<'_>) -> bool {
        let base = ctx.conflict.base_content.as_deref();
        let ours = ctx.conflict.ours_content.as_deref();
        let theirs = ctx.conflict.theirs_content.as_deref();

        // Guard: don't run diffy merge on oversized content (Myers is O(ND)).
        let any_oversized = [base, ours, theirs]
            .iter()
            .any(|c| c.is_some_and(|s| s.len() > MAX_DIFF_BYTE_SIZE));
        if any_oversized {
            return false;
        }

        let Some(merged_text) = build_conflict_marker_view(base, ours, theirs) else {
            return false;
        };

        let trunc_center = compute_truncation_center(base, ours, theirs);

        writeln!(
            out,
            "#### Conflict (diff3 markers \u{2014} OURS = trunk, THEIRS = agent's working directory)"
        )
        .unwrap();
        writeln!(out, "```{}", ctx.lang).unwrap();
        write_truncated(out, &merged_text, trunc_center);
        out.push('\n');
        writeln!(out, "```").unwrap();
        writeln!(out).unwrap();

        true
    }
}

/// Produce a single string with diff3-style conflict markers embedded at
/// divergence points.
///
/// Returns `None` if any of the three versions is missing (the caller should
/// use `MinimalFormat` instead).
pub(in crate::context_file::resolve) fn build_conflict_marker_view(
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
