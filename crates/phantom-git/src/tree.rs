//! Git tree building from blobs and overlay files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::GitError;
use crate::helpers::collect_files_recursive;

/// Build a new tree by layering `files` (with in-memory content) on top of
/// `base_tree`.
///
/// **Deprecated:** Prefer [`build_tree_from_oids`] with pre-created blobs to
/// avoid duplicating file content at each recursion level.
pub fn build_tree_with_blobs(
    repo: &git2::Repository,
    base_tree: &git2::Tree<'_>,
    files: &[(PathBuf, Vec<u8>)],
) -> Result<git2::Oid, GitError> {
    let mut root_blobs: Vec<(&std::ffi::OsStr, &[u8])> = Vec::new();
    let mut subdir_files: HashMap<&std::ffi::OsStr, Vec<(PathBuf, &[u8])>> = HashMap::new();

    for (path, content) in files {
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| GitError::MaterializationFailed("empty path".into()))?;

        let remaining: PathBuf = components.collect();
        let name = first.as_os_str();

        if remaining.as_os_str().is_empty() {
            root_blobs.push((name, content));
        } else {
            subdir_files
                .entry(name)
                .or_default()
                .push((remaining, content));
        }
    }

    let mut builder = repo.treebuilder(Some(base_tree))?;

    for (name, content) in &root_blobs {
        let blob_oid = repo.blob(content)?;
        builder.insert(
            name.to_str().ok_or_else(|| {
                GitError::MaterializationFailed("non-UTF-8 filename".into())
            })?,
            blob_oid,
            0o100644,
        )?;
    }

    for (dir_name, nested_files) in &subdir_files {
        let dir_name_str = dir_name.to_str().ok_or_else(|| {
            GitError::MaterializationFailed("non-UTF-8 directory name".into())
        })?;

        let existing_subtree = base_tree
            .get_name(dir_name_str)
            .and_then(|entry| entry.to_object(repo).ok())
            .and_then(|obj| obj.into_tree().ok());

        let owned_files: Vec<(PathBuf, Vec<u8>)> = nested_files
            .iter()
            .map(|(p, c)| (p.clone(), c.to_vec()))
            .collect();

        let subtree_oid = if let Some(ref sub) = existing_subtree {
            build_tree_with_blobs(repo, sub, &owned_files)?
        } else {
            let empty_oid = repo.treebuilder(None)?.write()?;
            let empty_tree = repo.find_tree(empty_oid)?;
            build_tree_with_blobs(repo, &empty_tree, &owned_files)?
        };

        builder.insert(dir_name_str, subtree_oid, 0o040000)?;
    }

    let tree_oid = builder.write()?;
    Ok(tree_oid)
}

/// Build a git tree by layering pre-created blob OIDs on top of `base_tree`.
///
/// This is the memory-efficient counterpart to [`build_tree_with_blobs`]:
/// callers create blobs up-front (one file at a time), then pass lightweight
/// `(PathBuf, git2::Oid)` pairs here. The recursive calls only clone the
/// path suffix and copy the 20-byte OID — no file-content duplication.
pub fn build_tree_from_oids(
    repo: &git2::Repository,
    base_tree: &git2::Tree<'_>,
    files: &[(PathBuf, git2::Oid)],
) -> Result<git2::Oid, GitError> {
    let mut root_blobs: Vec<(&std::ffi::OsStr, git2::Oid)> = Vec::new();
    let mut subdir_files: HashMap<&std::ffi::OsStr, Vec<(PathBuf, git2::Oid)>> = HashMap::new();

    for (path, oid) in files {
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| GitError::MaterializationFailed("empty path".into()))?;

        let remaining: PathBuf = components.collect();
        let name = first.as_os_str();

        if remaining.as_os_str().is_empty() {
            root_blobs.push((name, *oid));
        } else {
            subdir_files
                .entry(name)
                .or_default()
                .push((remaining, *oid));
        }
    }

    let mut builder = repo.treebuilder(Some(base_tree))?;

    for (name, oid) in &root_blobs {
        builder.insert(
            name.to_str().ok_or_else(|| {
                GitError::MaterializationFailed("non-UTF-8 filename".into())
            })?,
            *oid,
            0o100644,
        )?;
    }

    for (dir_name, nested_files) in &subdir_files {
        let dir_name_str = dir_name.to_str().ok_or_else(|| {
            GitError::MaterializationFailed("non-UTF-8 directory name".into())
        })?;

        let existing_subtree = base_tree
            .get_name(dir_name_str)
            .and_then(|entry| entry.to_object(repo).ok())
            .and_then(|obj| obj.into_tree().ok());

        let subtree_oid = if let Some(ref sub) = existing_subtree {
            build_tree_from_oids(repo, sub, nested_files)?
        } else {
            let empty_oid = repo.treebuilder(None)?.write()?;
            let empty_tree = repo.find_tree(empty_oid)?;
            build_tree_from_oids(repo, &empty_tree, nested_files)?
        };

        builder.insert(dir_name_str, subtree_oid, 0o040000)?;
    }

    let tree_oid = builder.write()?;
    Ok(tree_oid)
}

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
    fn test_build_tree_with_blobs_root_files() {
        let (dir, _ops) = init_repo(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]);
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = head.tree().unwrap();

        let files = vec![
            (PathBuf::from("a.txt"), b"modified-a".to_vec()),
            (PathBuf::from("c.txt"), b"new-c".to_vec()),
        ];

        let new_tree_oid = build_tree_with_blobs(&repo, &base_tree, &files).unwrap();
        let new_tree = repo.find_tree(new_tree_oid).unwrap();

        let a_blob = repo.find_blob(new_tree.get_name("a.txt").unwrap().id()).unwrap();
        assert_eq!(a_blob.content(), b"modified-a");

        let b_blob = repo.find_blob(new_tree.get_name("b.txt").unwrap().id()).unwrap();
        assert_eq!(b_blob.content(), b"bbb");

        let c_blob = repo.find_blob(new_tree.get_name("c.txt").unwrap().id()).unwrap();
        assert_eq!(c_blob.content(), b"new-c");
    }

    #[test]
    fn test_build_tree_with_blobs_nested_paths() {
        let (dir, _ops) =
            init_repo(&[("src/main.rs", b"fn main() {}"), ("src/lib.rs", b"pub mod lib;")]);
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = head.tree().unwrap();

        let files = vec![
            (PathBuf::from("src/main.rs"), b"fn main() { new }".to_vec()),
            (
                PathBuf::from("src/utils/helper.rs"),
                b"pub fn help() {}".to_vec(),
            ),
        ];

        let new_tree_oid = build_tree_with_blobs(&repo, &base_tree, &files).unwrap();
        let new_tree = repo.find_tree(new_tree_oid).unwrap();

        let src_tree = repo
            .find_tree(new_tree.get_name("src").unwrap().id())
            .unwrap();
        let main_blob = repo
            .find_blob(src_tree.get_name("main.rs").unwrap().id())
            .unwrap();
        assert_eq!(main_blob.content(), b"fn main() { new }");

        let lib_blob = repo
            .find_blob(src_tree.get_name("lib.rs").unwrap().id())
            .unwrap();
        assert_eq!(lib_blob.content(), b"pub mod lib;");

        let utils_tree = repo
            .find_tree(src_tree.get_name("utils").unwrap().id())
            .unwrap();
        let helper_blob = repo
            .find_blob(utils_tree.get_name("helper.rs").unwrap().id())
            .unwrap();
        assert_eq!(helper_blob.content(), b"pub fn help() {}");
    }

    #[test]
    fn test_build_tree_from_oids_root_files() {
        let (dir, _ops) = init_repo(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]);
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = head.tree().unwrap();

        let file_oids = vec![
            (PathBuf::from("a.txt"), repo.blob(b"modified-a").unwrap()),
            (PathBuf::from("c.txt"), repo.blob(b"new-c").unwrap()),
        ];

        let new_tree_oid = build_tree_from_oids(&repo, &base_tree, &file_oids).unwrap();
        let new_tree = repo.find_tree(new_tree_oid).unwrap();

        let a_blob = repo.find_blob(new_tree.get_name("a.txt").unwrap().id()).unwrap();
        assert_eq!(a_blob.content(), b"modified-a");

        let b_blob = repo.find_blob(new_tree.get_name("b.txt").unwrap().id()).unwrap();
        assert_eq!(b_blob.content(), b"bbb");

        let c_blob = repo.find_blob(new_tree.get_name("c.txt").unwrap().id()).unwrap();
        assert_eq!(c_blob.content(), b"new-c");
    }

    #[test]
    fn test_build_tree_from_oids_nested_paths() {
        let (dir, _ops) =
            init_repo(&[("src/main.rs", b"fn main() {}"), ("src/lib.rs", b"pub mod lib;")]);
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = head.tree().unwrap();

        let file_oids = vec![
            (
                PathBuf::from("src/main.rs"),
                repo.blob(b"fn main() { new }").unwrap(),
            ),
            (
                PathBuf::from("src/utils/helper.rs"),
                repo.blob(b"pub fn help() {}").unwrap(),
            ),
        ];

        let new_tree_oid = build_tree_from_oids(&repo, &base_tree, &file_oids).unwrap();
        let new_tree = repo.find_tree(new_tree_oid).unwrap();

        let src_tree = repo
            .find_tree(new_tree.get_name("src").unwrap().id())
            .unwrap();
        let main_blob = repo
            .find_blob(src_tree.get_name("main.rs").unwrap().id())
            .unwrap();
        assert_eq!(main_blob.content(), b"fn main() { new }");

        let lib_blob = repo
            .find_blob(src_tree.get_name("lib.rs").unwrap().id())
            .unwrap();
        assert_eq!(lib_blob.content(), b"pub mod lib;");

        let utils_tree = repo
            .find_tree(src_tree.get_name("utils").unwrap().id())
            .unwrap();
        let helper_blob = repo
            .find_blob(utils_tree.get_name("helper.rs").unwrap().id())
            .unwrap();
        assert_eq!(helper_blob.content(), b"pub fn help() {}");
    }

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
