//! `GitOps` methods for reading, writing, diffing, and reverting commits.

use std::path::{Path, PathBuf};

use phantom_core::id::GitOid;
use phantom_core::traits::MergeResult;
use tracing::info;

use super::{git_oid_to_oid, merge, oid_to_git_oid, GitOps};
use crate::error::OrchestratorError;

impl GitOps {
    /// Read the contents of `path` as it existed in the commit identified by `oid`.
    pub fn read_file_at_commit(
        &self,
        oid: &GitOid,
        path: &Path,
    ) -> Result<Vec<u8>, OrchestratorError> {
        let git_oid = git_oid_to_oid(oid)?;
        let commit = self.repo.find_commit(git_oid)?;
        let tree = commit.tree()?;

        let entry = tree.get_path(path).map_err(|_| {
            OrchestratorError::NotFound(format!("path not found in commit: {}", path.display()))
        })?;

        let blob = self.repo.find_blob(entry.id()).map_err(|_| {
            OrchestratorError::NotFound(format!("object at {} is not a blob", path.display()))
        })?;

        Ok(blob.content().to_vec())
    }

    /// List every blob path in the tree of the commit identified by `oid`.
    pub fn list_files_at_commit(&self, oid: &GitOid) -> Result<Vec<PathBuf>, OrchestratorError> {
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
    ) -> Result<GitOid, OrchestratorError> {
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
            return Err(OrchestratorError::MaterializationFailed(
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
    pub fn reset_to_commit(&self, oid: &GitOid) -> Result<(), OrchestratorError> {
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
    ) -> Result<Vec<PathBuf>, OrchestratorError> {
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
    ) -> Result<MergeResult, OrchestratorError> {
        merge::three_way_merge(base, ours, theirs)
    }
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
