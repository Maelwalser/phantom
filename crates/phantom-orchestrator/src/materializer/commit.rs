//! Build git commits from pre-created blob OIDs without touching the working tree.
//!
//! These helpers construct a new tree from the parent commit's tree plus a
//! set of file OIDs (and optional deletions), then commit it with `HEAD` as
//! the current reference.

use std::path::PathBuf;

use phantom_core::id::GitOid;

use crate::error::OrchestratorError;
use crate::git::{self, GitOps};

/// Build a commit from pre-created blob OIDs without touching the working tree.
pub(super) fn commit_from_oids(
    git: &GitOps,
    file_oids: &[(PathBuf, git2::Oid)],
    parent_oid: &GitOid,
    message: &str,
    author: &str,
) -> Result<GitOid, OrchestratorError> {
    commit_from_oids_with_deletions(git, file_oids, &[], parent_oid, message, author)
}

/// Build a commit from pre-created blob OIDs plus a list of deleted paths.
pub(super) fn commit_from_oids_with_deletions(
    git: &GitOps,
    file_oids: &[(PathBuf, git2::Oid)],
    deletions: &[PathBuf],
    parent_oid: &GitOid,
    message: &str,
    author: &str,
) -> Result<GitOid, OrchestratorError> {
    let repo = git.repo();
    let git2_parent_oid = git::git_oid_to_oid(parent_oid)?;
    let parent = repo.find_commit(git2_parent_oid)?;
    let base_tree = parent.tree()?;

    let new_tree_oid =
        git::build_tree_from_oids_with_deletions(repo, &base_tree, file_oids, deletions)?;
    let new_tree = repo.find_tree(new_tree_oid)?;

    let sig = git2::Signature::now(author, &format!("{author}@phantom"))?;
    let new_oid = repo.commit(Some("HEAD"), &sig, &sig, message, &new_tree, &[&parent])?;

    Ok(git::oid_to_git_oid(new_oid))
}
