//! `GitOps` methods for diffing commits and checking `.gitignore`.

use std::path::{Path, PathBuf};

use phantom_core::id::GitOid;

use crate::GitOps;
use crate::error::GitError;
use crate::oid::git_oid_to_oid;

impl GitOps {
    /// Return the list of file paths that differ between two commits.
    pub fn changed_files(&self, from: &GitOid, to: &GitOid) -> Result<Vec<PathBuf>, GitError> {
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

    /// Check if a path would be ignored by `.gitignore` rules.
    ///
    /// Uses `git2`'s full ignore chain: `.gitignore`, `.git/info/exclude`,
    /// and the user's global gitignore. Works for all languages/project types.
    pub fn is_ignored(&self, path: &Path) -> Result<bool, GitError> {
        Ok(self.repo.status_should_ignore(path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::test_helpers::init_repo_with_commit;

    #[test]
    fn test_changed_files() {
        let (_dir, ops) = init_repo_with_commit(&[("a.txt", b"aaa"), ("b.txt", b"bbb")], "init");
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
    fn test_is_ignored_respects_gitignore() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();

        // Write a .gitignore that ignores node_modules/ and target/
        std::fs::write(
            dir.path().join(".gitignore"),
            "node_modules/\ntarget/\n*.pyc\n",
        )
        .unwrap();

        // Stage and commit the .gitignore so it takes effect.
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new(".gitignore")).unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let sig = git2::Signature::now("test", "test@phantom").unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }

        let ops = GitOps::open(dir.path()).unwrap();

        // Gitignored paths
        assert!(
            ops.is_ignored(Path::new("node_modules/foo/index.js"))
                .unwrap()
        );
        assert!(ops.is_ignored(Path::new("target/debug/build")).unwrap());
        assert!(ops.is_ignored(Path::new("lib/cache.pyc")).unwrap());

        // Non-ignored paths
        assert!(!ops.is_ignored(Path::new("src/main.rs")).unwrap());
        assert!(!ops.is_ignored(Path::new("package.json")).unwrap());
    }
}
