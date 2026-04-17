//! `GitOps::text_merge` — three-way line-based text merge.

use phantom_core::MergeResult;

use crate::GitOps;
use crate::error::GitError;
use crate::merge;

impl GitOps {
    /// Perform a line-based three-way merge.
    ///
    /// Returns [`MergeResult::Clean`] with the merged bytes on success, or
    /// [`MergeResult::Conflict`] with a [`phantom_core::conflict::ConflictDetail`]
    /// if the same region was modified on both sides.
    pub fn text_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
    ) -> Result<MergeResult, GitError> {
        merge::three_way_merge(base, ours, theirs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::test_helpers::init_repo_with_commit;
    use phantom_core::conflict::ConflictKind;

    #[test]
    fn test_text_merge_clean() {
        let (_dir, ops) = init_repo_with_commit(&[("x.txt", b"x")], "init");

        let base = b"a\nb\nc\nd\n";
        let ours = b"a\nB\nc\nd\n";
        let theirs = b"a\nb\nc\nD\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8(merged).unwrap();
                assert!(text.contains('B'), "should contain ours' change");
                assert!(text.contains('D'), "should contain theirs' change");
            }
            MergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_text_merge_conflict() {
        let (_dir, ops) = init_repo_with_commit(&[("x.txt", b"x")], "init");

        let base = b"a\nb\nc\n";
        let ours = b"a\nX\nc\n";
        let theirs = b"a\nY\nc\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Clean(_) => panic!("expected conflict"),
            MergeResult::Conflict(details) => {
                assert!(!details.is_empty());
            }
        }
    }

    #[test]
    fn test_text_merge_rejects_binary() {
        let (_dir, ops) = init_repo_with_commit(&[("a.bin", b"init")], "init");
        let base = b"some text\n";
        let ours = b"some\x00binary\n";
        let theirs = b"other text\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }

    #[test]
    fn test_text_merge_rejects_non_utf8() {
        let (_dir, ops) = init_repo_with_commit(&[("a.txt", b"init")], "init");
        let base = b"hello\n";
        let ours = b"hello\n";
        let theirs = b"\xff\xfe\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }
}
