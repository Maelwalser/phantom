//! `GitOps` methods for mutating history: revert and hard reset.

use phantom_core::id::GitOid;
use tracing::info;

use crate::GitOps;
use crate::error::GitError;
use crate::oid::{git_oid_to_oid, oid_to_git_oid};

impl GitOps {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::test_helpers::init_repo_with_commit;

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
    fn test_recovery_failure_reported() {
        let err = GitError::MaterializationFailed("copy failed: disk full".into());
        let msg = err.to_string();
        assert!(msg.contains("disk full"), "error was: {msg}");
    }
}
