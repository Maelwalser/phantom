//! LCS-based three-way text merge fallback using `diffy`.

use std::path::Path;

use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::error::CoreError;
use phantom_core::id::ChangesetId;
use phantom_core::is_binary_or_non_utf8;
use phantom_core::traits::MergeResult;

/// LCS-based three-way text merge fallback.
///
/// Correctly handles insertions, deletions, and modifications at arbitrary
/// positions. Falls back to conflict when both sides change the same region.
pub(super) fn text_merge(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    path: &Path,
) -> Result<MergeResult, CoreError> {
    // Reject binary or non-UTF-8 content to prevent silent data corruption.
    if is_binary_or_non_utf8(base) || is_binary_or_non_utf8(ours) || is_binary_or_non_utf8(theirs) {
        return Ok(MergeResult::Conflict(vec![ConflictDetail {
            kind: ConflictKind::BinaryFile,
            file: path.to_path_buf(),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "file is binary or not valid UTF-8; cannot text-merge".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }]));
    }

    // Safe: all three buffers validated as UTF-8 above.
    let base_str = std::str::from_utf8(base).unwrap();
    let ours_str = std::str::from_utf8(ours).unwrap();
    let theirs_str = std::str::from_utf8(theirs).unwrap();

    match diffy::merge(base_str, ours_str, theirs_str) {
        Ok(merged) => Ok(MergeResult::Clean(merged.into_bytes())),
        Err(_conflict_text) => Ok(MergeResult::Conflict(vec![ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: path.to_path_buf(),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "line-level text conflict".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }])),
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
