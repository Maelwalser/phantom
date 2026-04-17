//! Shared test helpers for the `ops` sub-modules.

use std::path::Path;

use crate::GitOps;

/// Create a temporary git repo with an initial commit containing `files`.
pub(crate) fn init_repo_with_commit(
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
