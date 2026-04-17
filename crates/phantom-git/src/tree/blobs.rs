//! Blob creation from overlay directories or in-memory content.

use std::path::{Path, PathBuf};

use crate::error::GitError;
use crate::fs_walk::collect_files_recursive;

/// Read overlay files one at a time, create blobs immediately, and return
/// `(path, blob_oid)` pairs. Peak memory is one file's content at a time,
/// rather than all files simultaneously.
pub fn create_blobs_from_overlay(
    repo: &git2::Repository,
    upper_dir: &Path,
) -> Result<Vec<(PathBuf, git2::Oid)>, GitError> {
    let paths = collect_files_recursive(upper_dir)?;
    let mut file_oids = Vec::with_capacity(paths.len());
    for rel_path in paths {
        let blob_oid = repo.blob_path(&upper_dir.join(&rel_path))?;
        file_oids.push((rel_path, blob_oid));
    }
    Ok(file_oids)
}

/// Convert in-memory `(path, content)` pairs into `(path, blob_oid)` pairs
/// by creating git blob objects. Used by the merge path where content is
/// already in memory from three-way merge results.
pub fn create_blobs_from_content(
    repo: &git2::Repository,
    files: &[(PathBuf, Vec<u8>)],
) -> Result<Vec<(PathBuf, git2::Oid)>, GitError> {
    let mut file_oids = Vec::with_capacity(files.len());
    for (path, content) in files {
        let blob_oid = repo.blob(content)?;
        file_oids.push((path.clone(), blob_oid));
    }
    Ok(file_oids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_repo;

    #[test]
    fn test_create_blobs_from_content() {
        let (dir, _ops) = init_repo(&[("x.txt", b"x")]);
        let repo = git2::Repository::open(dir.path()).unwrap();

        let files = vec![
            (PathBuf::from("a.txt"), b"aaa".to_vec()),
            (PathBuf::from("b.txt"), b"bbb".to_vec()),
        ];

        let oids = create_blobs_from_content(&repo, &files).unwrap();
        assert_eq!(oids.len(), 2);
        assert_eq!(oids[0].0, PathBuf::from("a.txt"));
        assert_eq!(oids[1].0, PathBuf::from("b.txt"));

        let a_blob = repo.find_blob(oids[0].1).unwrap();
        assert_eq!(a_blob.content(), b"aaa");
        let b_blob = repo.find_blob(oids[1].1).unwrap();
        assert_eq!(b_blob.content(), b"bbb");
    }

    #[test]
    fn test_create_blobs_from_overlay() {
        let (dir, _ops) = init_repo(&[("x.txt", b"x")]);
        let repo = git2::Repository::open(dir.path()).unwrap();

        let upper = tempfile::TempDir::new().unwrap();
        std::fs::write(upper.path().join("hello.txt"), b"hello").unwrap();
        std::fs::create_dir(upper.path().join("sub")).unwrap();
        std::fs::write(upper.path().join("sub/nested.txt"), b"nested").unwrap();

        let oids = create_blobs_from_overlay(&repo, upper.path()).unwrap();
        assert_eq!(oids.len(), 2);

        for (path, oid) in &oids {
            let blob = repo.find_blob(*oid).unwrap();
            if path == &PathBuf::from("hello.txt") {
                assert_eq!(blob.content(), b"hello");
            } else if path == &PathBuf::from("sub/nested.txt") {
                assert_eq!(blob.content(), b"nested");
            } else {
                panic!("unexpected path: {path:?}");
            }
        }
    }
}
