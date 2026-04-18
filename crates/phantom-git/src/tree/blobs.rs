//! Blob creation from overlay directories or in-memory content.

use std::path::{Path, PathBuf};

use crate::error::GitError;
use crate::fs_walk::collect_files_recursive;

/// Git blob mode for a regular (non-executable) file.
pub const MODE_REGULAR: u32 = 0o100_644;
/// Git blob mode for an executable regular file.
pub const MODE_EXECUTABLE: u32 = 0o100_755;
/// Git mode for a symbolic link.
pub const MODE_SYMLINK: u32 = 0o120_000;

/// Read overlay files one at a time, create blobs immediately, and return
/// `(path, blob_oid, mode)` tuples. Peak memory is one file's content at a
/// time, rather than all files simultaneously.
///
/// The `mode` component is derived from the file's `st_mode` so the
/// executable bit and symlink-ness survive the round trip into the tree.
/// Prior to this, every blob was inserted with mode `100644`, which
/// silently stripped the execute bit from committed files such as shell
/// scripts and cargo's `build_script_build` binaries.
///
/// Uses `fs::read` + `repo.blob(bytes)` instead of `repo.blob_path(path)`
/// so we don't stream bytes through libgit2 while an agent may still be
/// writing the file. An `InvalidData` / `Interrupted` short read from a
/// concurrent rewrite is retried once before propagating.
pub fn create_blobs_from_overlay(
    repo: &git2::Repository,
    upper_dir: &Path,
) -> Result<Vec<(PathBuf, git2::Oid, u32)>, GitError> {
    let paths = collect_files_recursive(upper_dir)?;
    let mut file_oids = Vec::with_capacity(paths.len());
    for rel_path in paths {
        let abs = upper_dir.join(&rel_path);
        let mode = derive_mode_from_fs(&abs)?;
        if mode == MODE_SYMLINK {
            // libgit2 expects the blob content for a symlink to be the
            // link target bytes, not the file at the target path.
            let target = std::fs::read_link(&abs)?;
            let blob_oid = repo.blob(target.as_os_str().to_string_lossy().as_bytes())?;
            file_oids.push((rel_path, blob_oid, mode));
            continue;
        }
        let content = read_with_retry(&abs)?;
        let blob_oid = repo.blob(&content)?;
        file_oids.push((rel_path, blob_oid, mode));
    }
    Ok(file_oids)
}

#[cfg(unix)]
fn derive_mode_from_fs(abs: &Path) -> Result<u32, GitError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(abs)?;
    if meta.file_type().is_symlink() {
        return Ok(MODE_SYMLINK);
    }
    if meta.permissions().mode() & 0o111 != 0 {
        Ok(MODE_EXECUTABLE)
    } else {
        Ok(MODE_REGULAR)
    }
}

#[cfg(not(unix))]
fn derive_mode_from_fs(_abs: &Path) -> Result<u32, GitError> {
    // Non-unix platforms: fall back to regular mode. Git stores modes as
    // git-mode integers regardless of host, but only unix exposes the
    // executable bit to query.
    Ok(MODE_REGULAR)
}

fn read_with_retry(path: &Path) -> Result<Vec<u8>, GitError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::InvalidData
            ) =>
        {
            Ok(std::fs::read(path)?)
        }
        Err(e) => Err(e.into()),
    }
}

/// Convert in-memory `(path, content, mode)` tuples into `(path, oid, mode)`
/// tuples by creating git blob objects. Used by the merge path where
/// content is already in memory from three-way merge results.
///
/// The caller provides the mode explicitly because the in-memory merge
/// output carries no filesystem metadata; upstream merge code looks up
/// each file's original mode from the base tree before calling here.
pub fn create_blobs_from_content(
    repo: &git2::Repository,
    files: &[(PathBuf, Vec<u8>, u32)],
) -> Result<Vec<(PathBuf, git2::Oid, u32)>, GitError> {
    let mut file_oids = Vec::with_capacity(files.len());
    for (path, content, mode) in files {
        let blob_oid = repo.blob(content)?;
        file_oids.push((path.clone(), blob_oid, *mode));
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
            (PathBuf::from("a.txt"), b"aaa".to_vec(), MODE_REGULAR),
            (PathBuf::from("b.txt"), b"bbb".to_vec(), MODE_EXECUTABLE),
        ];

        let oids = create_blobs_from_content(&repo, &files).unwrap();
        assert_eq!(oids.len(), 2);
        assert_eq!(oids[0].0, PathBuf::from("a.txt"));
        assert_eq!(oids[0].2, MODE_REGULAR);
        assert_eq!(oids[1].0, PathBuf::from("b.txt"));
        assert_eq!(oids[1].2, MODE_EXECUTABLE);

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

        for (path, oid, mode) in &oids {
            // Non-executable text files should come back as regular mode.
            assert_eq!(
                *mode,
                MODE_REGULAR,
                "unexpected mode for {}",
                path.display()
            );
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

    #[cfg(unix)]
    #[test]
    fn test_create_blobs_from_overlay_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, _ops) = init_repo(&[("x.txt", b"x")]);
        let repo = git2::Repository::open(dir.path()).unwrap();

        let upper = tempfile::TempDir::new().unwrap();
        let script_path = upper.path().join("run.sh");
        std::fs::write(&script_path, b"#!/bin/sh\necho hi\n").unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();

        let oids = create_blobs_from_overlay(&repo, upper.path()).unwrap();
        let (_path, _oid, mode) = oids
            .iter()
            .find(|(p, _, _)| p == &PathBuf::from("run.sh"))
            .expect("run.sh blob missing");
        assert_eq!(
            *mode, MODE_EXECUTABLE,
            "executable bit must be preserved so the committed file stays executable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_create_blobs_from_overlay_emits_symlink_mode() {
        let (dir, _ops) = init_repo(&[("x.txt", b"x")]);
        let repo = git2::Repository::open(dir.path()).unwrap();

        let upper = tempfile::TempDir::new().unwrap();
        // Create a target and a symlink pointing at it. The overlay upper
        // is the containing dir; we only walk files inside it.
        std::fs::write(upper.path().join("target.txt"), b"real").unwrap();
        std::os::unix::fs::symlink("target.txt", upper.path().join("link.txt")).unwrap();

        let oids = create_blobs_from_overlay(&repo, upper.path()).unwrap();
        let link = oids
            .iter()
            .find(|(p, _, _)| p == &PathBuf::from("link.txt"))
            .expect("link.txt missing");
        assert_eq!(link.2, MODE_SYMLINK);
        let blob = repo.find_blob(link.1).unwrap();
        // Git stores a symlink's blob as its raw target path.
        assert_eq!(blob.content(), b"target.txt");
    }
}
