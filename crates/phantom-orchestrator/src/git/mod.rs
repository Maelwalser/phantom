//! Git operations for Phantom, built on `git2`.
//!
//! Provides [`GitOps`] — a wrapper around a `git2::Repository` — and
//! lossless conversions between [`phantom_core::GitOid`] and [`git2::Oid`].

use std::path::Path;

use phantom_core::id::GitOid;

use crate::error::OrchestratorError;

pub(crate) mod helpers;
mod merge;
mod ops;
pub(crate) mod tree;

// Re-export public API from submodules.
pub use tree::{build_tree_from_oids, create_blobs_from_content};
pub(crate) use tree::create_blobs_from_overlay;
#[cfg(test)]
pub use tree::build_tree_with_blobs;

// ---------------------------------------------------------------------------
// GitOid <-> git2::Oid conversions
// ---------------------------------------------------------------------------

/// Convert a `git2::Oid` into a `GitOid`.
#[must_use]
pub fn oid_to_git_oid(oid: git2::Oid) -> GitOid {
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(oid.as_bytes());
    GitOid(bytes)
}

/// Convert a `GitOid` into a `git2::Oid`.
pub fn git_oid_to_oid(oid: &GitOid) -> Result<git2::Oid, git2::Error> {
    git2::Oid::from_bytes(&oid.0)
}

// ---------------------------------------------------------------------------
// GitOps
// ---------------------------------------------------------------------------

/// Thin wrapper around a `git2::Repository` exposing the operations Phantom
/// needs: reading files, committing overlay changes, resetting, and diffing.
pub struct GitOps {
    pub(crate) repo: git2::Repository,
}

impl GitOps {
    /// Open an existing git repository at `repo_path`.
    #[must_use = "returns a Result that should be checked"]
    pub fn open(repo_path: &Path) -> Result<Self, OrchestratorError> {
        let repo = git2::Repository::open(repo_path)?;
        Ok(Self { repo })
    }

    /// Borrow the inner `git2::Repository` for advanced operations.
    pub fn repo(&self) -> &git2::Repository {
        &self.repo
    }

    /// Return the OID of the commit that `HEAD` points to.
    ///
    /// Returns [`GitOid::zero()`] when `HEAD` is unborn (empty repository with
    /// no commits).
    pub fn head_oid(&self) -> Result<GitOid, OrchestratorError> {
        match self.repo.head() {
            Ok(head) => {
                let oid = head
                    .target()
                    .ok_or_else(|| OrchestratorError::NotFound("HEAD has no target".into()))?;
                Ok(oid_to_git_oid(oid))
            }
            Err(e) if e.code() == git2::ErrorCode::UnbornBranch => Ok(GitOid::zero()),
            Err(e) => Err(OrchestratorError::Git(e)),
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
