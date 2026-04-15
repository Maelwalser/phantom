//! LCS-based three-way text merge (via `diffy`).

use std::path::PathBuf;

use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::id::ChangesetId;
use phantom_core::is_binary_or_non_utf8;
use phantom_core::traits::MergeResult;

use crate::error::GitError;

/// Three-way merge using LCS-based diff alignment.
///
/// Computes the diff between base->ours and base->theirs independently,
/// then merges the changes. Correctly handles insertions and deletions
/// at arbitrary positions.
pub(crate) fn three_way_merge(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
) -> Result<MergeResult, GitError> {
    // Reject binary or non-UTF-8 content to prevent silent data corruption.
    if is_binary_or_non_utf8(base) || is_binary_or_non_utf8(ours) || is_binary_or_non_utf8(theirs) {
        let detail = ConflictDetail {
            kind: ConflictKind::BinaryFile,
            file: PathBuf::from("<text-merge>"),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "file is binary or not valid UTF-8; cannot text-merge".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        };
        return Ok(MergeResult::Conflict(vec![detail]));
    }

    // All three buffers were validated above, but use proper error propagation
    // rather than unwrap to stay robust against future changes in the guard.
    let base_s = std::str::from_utf8(base).map_err(|e| {
        GitError::MaterializationFailed(format!("base is not valid UTF-8: {e}"))
    })?;
    let ours_s = std::str::from_utf8(ours).map_err(|e| {
        GitError::MaterializationFailed(format!("ours is not valid UTF-8: {e}"))
    })?;
    let theirs_s = std::str::from_utf8(theirs).map_err(|e| {
        GitError::MaterializationFailed(format!("theirs is not valid UTF-8: {e}"))
    })?;

    let result = diffy::merge(base_s, ours_s, theirs_s);
    match result {
        Ok(merged) => Ok(MergeResult::Clean(merged.into_bytes())),
        Err(conflict_text) => {
            // diffy returns the conflicted text with markers.
            // We report this as a conflict.
            let detail = ConflictDetail {
                kind: ConflictKind::RawTextConflict,
                file: PathBuf::from("<text-merge>"),
                symbol_id: None,
                ours_changeset: ChangesetId("unknown".into()),
                theirs_changeset: ChangesetId("unknown".into()),
                description: "line-based three-way merge produced conflicts".into(),
                ours_span: None,
                theirs_span: None,
                base_span: None,
            };
            let _ = conflict_text; // conflict markers available if needed
            Ok(MergeResult::Conflict(vec![detail]))
        }
    }
}
