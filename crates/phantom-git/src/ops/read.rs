//! `GitOps` methods for reading files and listing tree contents at a commit.

use std::path::{Path, PathBuf};

use phantom_core::id::GitOid;

use crate::GitOps;
use crate::error::GitError;
use crate::oid::git_oid_to_oid;

impl GitOps {
    /// Read the contents of `path` as it existed in the commit identified by `oid`.
    pub fn read_file_at_commit(&self, oid: &GitOid, path: &Path) -> Result<Vec<u8>, GitError> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::test_helpers::init_repo_with_commit;

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
}
