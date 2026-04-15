//! LCS-based three-way text merge fallback using `diffy`.

use std::path::Path;

use phantom_core::conflict::{ConflictDetail, ConflictKind};
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
) -> MergeResult {
    // Reject binary or non-UTF-8 content to prevent silent data corruption.
    if is_binary_or_non_utf8(base) || is_binary_or_non_utf8(ours) || is_binary_or_non_utf8(theirs) {
        return MergeResult::Conflict(vec![ConflictDetail {
            kind: ConflictKind::BinaryFile,
            file: path.to_path_buf(),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "file is binary or not valid UTF-8; cannot text-merge".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }]);
    }

    // Safe: all three buffers validated as UTF-8 above.
    let base_str = std::str::from_utf8(base).unwrap();
    let ours_str = std::str::from_utf8(ours).unwrap();
    let theirs_str = std::str::from_utf8(theirs).unwrap();

    match diffy::merge(base_str, ours_str, theirs_str) {
        Ok(merged) => MergeResult::Clean(merged.into_bytes()),
        Err(_conflict_text) => MergeResult::Conflict(vec![ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: path.to_path_buf(),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "line-level text conflict".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        }]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_file_with_null_bytes_returns_conflict() {
        let base = b"line1\nline2\n";
        let ours = b"line1\x00binary\nline2\n";
        let theirs = b"line1\nline2\nline3\n";

        let result = text_merge(base, ours, theirs, Path::new("data.bin"));

        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }

    #[test]
    fn non_utf8_bytes_returns_conflict() {
        let base = b"valid utf8\n";
        let ours = b"\xff\xfe invalid utf8\n";
        let theirs = b"also valid\n";

        let result = text_merge(base, ours, theirs, Path::new("encoded.txt"));

        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts.len(), 1);
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }

    #[test]
    fn valid_utf8_text_merges_normally() {
        let base = b"line1\nline2\nline3\n";
        let ours = b"line1\nmodified\nline3\n";
        let theirs = b"line1\nline2\nline3\nline4\n";

        let result = text_merge(base, ours, theirs, Path::new("notes.txt"));

        match result {
            MergeResult::Clean(merged) => {
                let text = std::str::from_utf8(&merged).unwrap();
                assert!(text.contains("modified"));
                assert!(text.contains("line4"));
            }
            MergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }
}
