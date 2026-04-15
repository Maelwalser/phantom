//! `GitOps` methods for reading, writing, diffing, and reverting commits.

use std::path::{Path, PathBuf};

use phantom_core::id::GitOid;
use phantom_core::traits::MergeResult;
use tracing::info;

use super::{git_oid_to_oid, merge, oid_to_git_oid, GitOps};
use crate::error::GitError;

impl GitOps {
    /// Read the contents of `path` as it existed in the commit identified by `oid`.
    pub fn read_file_at_commit(
        &self,
        oid: &GitOid,
        path: &Path,
    ) -> Result<Vec<u8>, GitError> {
        let git_oid = git_oid_to_oid(oid)?;
        let commit = self.repo.find_commit(git_oid)?;
        let tree = commit.tree()?;

        let entry = tree.get_path(path).map_err(|_| {
            GitError::NotFound(format!("path not found in commit: {}", path.display()))
        })?;

        let blob = self.repo.find_blob(entry.id()).map_err(|_| {
            GitError::NotFound(format!("object at {} is not a blob", path.display()))
        })?;

        Ok(blob.content().to_vec())
    }

    /// List every blob path in the tree of the commit identified by `oid`.
    pub fn list_files_at_commit(&self, oid: &GitOid) -> Result<Vec<PathBuf>, GitError> {
        let git_oid = git_oid_to_oid(oid)?;
        let commit = self.repo.find_commit(git_oid)?;
        let tree = commit.tree()?;

        let mut paths = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob) {
                let full = if dir.is_empty() {
                    PathBuf::from(entry.name().unwrap_or(""))
                } else {
                    PathBuf::from(dir).join(entry.name().unwrap_or(""))
                };
                paths.push(full);
            }
            git2::TreeWalkResult::Ok
        })?;

        Ok(paths)
    }

    /// Revert a specific commit by creating a new commit that undoes its
    /// changes.
    ///
    /// This is the inverse of cherry-pick: it computes what the tree would look
    /// like if the given commit had never been applied, then commits that tree
    /// on top of the current HEAD. Subsequent commits are preserved.
    ///
    /// Returns the OID of the newly created revert commit, or an error if the
    /// revert produces conflicts (e.g. the changes were modified by a later
    /// commit).
    pub fn revert_commit_oid(
        &self,
        commit_oid: &GitOid,
        message: &str,
    ) -> Result<GitOid, GitError> {
        let git_oid = git_oid_to_oid(commit_oid)?;
        let revert_commit = self.repo.find_commit(git_oid)?;

        let head_oid_val = self.head_oid()?;
        let head_git_oid = git_oid_to_oid(&head_oid_val)?;
        let our_commit = self.repo.find_commit(head_git_oid)?;

        // mainline = 0 for non-merge commits
        let mut index = self
            .repo
            .revert_commit(&revert_commit, &our_commit, 0, None)?;

        if index.has_conflicts() {
            return Err(GitError::MaterializationFailed(
                "revert produced conflicts — the rolled-back changes were modified by a later commit".into(),
            ));
        }

        let tree_oid = index.write_tree_to(&self.repo)?;
        let tree = self.repo.find_tree(tree_oid)?;
        let sig = git2::Signature::now("phantom", "phantom@rollback")?;

        let new_oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&our_commit])?;

        // Update working directory to match
        self.repo
            .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;

        info!(reverted = %commit_oid, new_commit = %new_oid, "reverted commit");
        Ok(oid_to_git_oid(new_oid))
    }

    /// Hard-reset: move `HEAD` to `oid` and update the index and working tree
    /// to match.
    pub fn reset_to_commit(&self, oid: &GitOid) -> Result<(), GitError> {
        let git_oid = git_oid_to_oid(oid)?;
        let commit = self.repo.find_commit(git_oid)?;
        let obj = commit.as_object();
        self.repo.reset(obj, git2::ResetType::Hard, None)?;
        Ok(())
    }

    /// Return the list of file paths that differ between two commits.
    pub fn changed_files(
        &self,
        from: &GitOid,
        to: &GitOid,
    ) -> Result<Vec<PathBuf>, GitError> {
        let from_oid = git_oid_to_oid(from)?;
        let to_oid = git_oid_to_oid(to)?;

        let from_tree = self.repo.find_commit(from_oid)?.tree()?;
        let to_tree = self.repo.find_commit(to_oid)?.tree()?;

        let diff = self
            .repo
            .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)?;

        let mut paths = Vec::new();
        diff.foreach(
            &mut |delta, _progress| {
                if let Some(p) = delta.new_file().path() {
                    paths.push(p.to_path_buf());
                } else if let Some(p) = delta.old_file().path() {
                    paths.push(p.to_path_buf());
                }
                true
            },
            None,
            None,
            None,
        )?;

        Ok(paths)
    }

    /// Perform a line-based three-way merge.
    ///
    /// Returns [`MergeResult::Clean`] with the merged bytes on success, or
    /// [`MergeResult::Conflict`] with a [`ConflictDetail`] if the same region
    /// was modified on both sides.
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
    use phantom_core::conflict::ConflictKind;

    use crate::test_support::init_repo;

    fn init_repo_with_commit(
        files: &[(&str, &[u8])],
        message: &str,
    ) -> (tempfile::TempDir, GitOps) {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        {
            let mut index = repo.index().unwrap();
            for &(path, content) in files {
                let full = dir.path().join(path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&full, content).unwrap();
                index.add_path(Path::new(path)).unwrap();
            }
            index.write().unwrap();

            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let sig = git2::Signature::now("test", "test@phantom").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
                .unwrap();
        }

        let ops = GitOps::open(dir.path()).unwrap();
        (dir, ops)
    }

    #[test]
    fn test_open_and_head_oid() {
        let (dir, ops) = init_repo_with_commit(&[("a.txt", b"hello")], "init");
        let oid = ops.head_oid().unwrap();
        assert_ne!(oid, GitOid::zero());

        let ops2 = GitOps::open(dir.path()).unwrap();
        assert_eq!(ops2.head_oid().unwrap(), oid);
    }

    #[test]
    fn test_head_oid_unborn() {
        let dir = tempfile::TempDir::new().unwrap();
        let _repo = git2::Repository::init(dir.path()).unwrap();
        let ops = GitOps::open(dir.path()).unwrap();
        assert_eq!(ops.head_oid().unwrap(), GitOid::zero());
    }

    #[test]
    fn test_read_file_at_commit() {
        let content = b"fn main() {}";
        let (_dir, ops) = init_repo_with_commit(&[("src/main.rs", content)], "init");
        let oid = ops.head_oid().unwrap();
        let read = ops
            .read_file_at_commit(&oid, Path::new("src/main.rs"))
            .unwrap();
        assert_eq!(read, content);
    }

    #[test]
    fn test_read_file_not_found() {
        let (_dir, ops) = init_repo_with_commit(&[("a.txt", b"x")], "init");
        let oid = ops.head_oid().unwrap();
        let result = ops.read_file_at_commit(&oid, Path::new("nonexistent.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn test_list_files_at_commit() {
        let files = &[
            ("README.md", b"# phantom" as &[u8]),
            ("src/main.rs", b"fn main() {}"),
            ("src/lib/util.rs", b"pub fn helper() {}"),
        ];
        let (_dir, ops) = init_repo_with_commit(files, "init");
        let oid = ops.head_oid().unwrap();
        let mut listed = ops.list_files_at_commit(&oid).unwrap();
        listed.sort();

        let mut expected: Vec<PathBuf> = files.iter().map(|(p, _)| PathBuf::from(p)).collect();
        expected.sort();

        assert_eq!(listed, expected);
    }

    #[test]
    fn test_commit_overlay_changes() {
        let (_dir, ops) = init_repo_with_commit(&[("src/main.rs", b"fn main() {}")], "init");
        let old_oid = ops.head_oid().unwrap();

        let upper = tempfile::TempDir::new().unwrap();
        let upper_main = upper.path().join("src/main.rs");
        std::fs::create_dir_all(upper_main.parent().unwrap()).unwrap();
        std::fs::write(&upper_main, b"fn main() { println!(\"hi\"); }").unwrap();

        let upper_lib = upper.path().join("src/lib.rs");
        std::fs::write(&upper_lib, b"pub fn greet() {}").unwrap();

        let trunk_path = ops.repo().workdir().unwrap().to_path_buf();
        let new_oid = ops
            .commit_overlay_changes(upper.path(), &trunk_path, "overlay commit", "agent-a")
            .unwrap();

        assert_ne!(old_oid, new_oid);
        assert_eq!(ops.head_oid().unwrap(), new_oid);

        let main_content = ops
            .read_file_at_commit(&new_oid, Path::new("src/main.rs"))
            .unwrap();
        assert_eq!(main_content, b"fn main() { println!(\"hi\"); }");

        let lib_content = ops
            .read_file_at_commit(&new_oid, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(lib_content, b"pub fn greet() {}");
    }

    #[test]
    fn test_reset_to_commit() {
        let (_dir, ops) = init_repo_with_commit(&[("a.txt", b"v1")], "commit 1");
        let first_oid = ops.head_oid().unwrap();

        let trunk = ops.repo().workdir().unwrap().to_path_buf();

        let upper2 = tempfile::TempDir::new().unwrap();
        std::fs::write(upper2.path().join("a.txt"), b"v2").unwrap();
        let second_oid = ops
            .commit_overlay_changes(upper2.path(), &trunk, "commit 2", "test")
            .unwrap();

        let upper3 = tempfile::TempDir::new().unwrap();
        std::fs::write(upper3.path().join("a.txt"), b"v3").unwrap();
        let _third_oid = ops
            .commit_overlay_changes(upper3.path(), &trunk, "commit 3", "test")
            .unwrap();

        assert_ne!(ops.head_oid().unwrap(), first_oid);

        ops.reset_to_commit(&first_oid).unwrap();
        assert_eq!(ops.head_oid().unwrap(), first_oid);

        ops.reset_to_commit(&second_oid).unwrap();
        assert_eq!(ops.head_oid().unwrap(), second_oid);
    }

    #[test]
    fn test_changed_files() {
        let (_dir, ops) =
            init_repo_with_commit(&[("a.txt", b"aaa"), ("b.txt", b"bbb")], "init");
        let first_oid = ops.head_oid().unwrap();

        let trunk = ops.repo().workdir().unwrap().to_path_buf();
        let upper = tempfile::TempDir::new().unwrap();
        std::fs::write(upper.path().join("a.txt"), b"aaa-modified").unwrap();
        std::fs::write(upper.path().join("c.txt"), b"ccc").unwrap();
        let second_oid = ops
            .commit_overlay_changes(upper.path(), &trunk, "modify", "test")
            .unwrap();

        let mut changed = ops.changed_files(&first_oid, &second_oid).unwrap();
        changed.sort();

        assert_eq!(
            changed,
            vec![PathBuf::from("a.txt"), PathBuf::from("c.txt")]
        );
    }

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

    #[test]
    fn test_recovery_failure_reported() {
        let err = GitError::MaterializationFailed("copy failed: disk full".into());
        let msg = err.to_string();
        assert!(msg.contains("disk full"), "error was: {msg}");
    }
}
