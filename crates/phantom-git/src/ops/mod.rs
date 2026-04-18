//! `GitOps` — a thin wrapper around a `git2::Repository` exposing the
//! operations Phantom needs.
//!
//! The `impl GitOps` blocks are split across sibling modules by concern:
//! - `read` — reading files and listing tree contents at a commit
//! - `history` — mutating history (revert, hard reset)
//! - `diff` — diffing commits and checking `.gitignore`
//! - `merge` — three-way text merge (thin wrapper over the crate-level `merge` module)

use std::path::Path;

use phantom_core::id::GitOid;

use crate::error::GitError;
use crate::oid::oid_to_git_oid;

mod diff;
mod history;
mod merge;
mod read;

#[cfg(test)]
pub(crate) mod test_helpers;

/// Thin wrapper around a `git2::Repository` exposing the operations Phantom
/// needs: reading files, committing overlay changes, resetting, and diffing.
pub struct GitOps {
    pub(crate) repo: git2::Repository,
}

impl GitOps {
    /// Open an existing git repository at `repo_path`.
    #[must_use = "returns a Result that should be checked"]
    pub fn open(repo_path: &Path) -> Result<Self, GitError> {
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
    pub fn head_oid(&self) -> Result<GitOid, GitError> {
        match self.repo.head() {
            Ok(head) => {
                let oid = head
                    .target()
                    .ok_or_else(|| GitError::NotFound("HEAD has no target".into()))?;
                Ok(oid_to_git_oid(oid))
            }
            Err(e) if e.code() == git2::ErrorCode::UnbornBranch => Ok(GitOid::zero()),
            Err(e) => Err(GitError::Git(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::test_helpers::init_repo_with_commit;

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
}
