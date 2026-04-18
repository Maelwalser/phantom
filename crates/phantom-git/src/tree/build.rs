//! Git tree building from pre-created blob OIDs.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::error::GitError;

/// Build a git tree by layering pre-created blob OIDs on top of `base_tree`.
///
/// Callers create blobs up-front (one file at a time), then pass lightweight
/// `(PathBuf, git2::Oid, u32)` tuples here. The `u32` is the git file mode
/// (`0o100644` regular, `0o100755` executable, `0o120000` symlink) — it
/// travels with each blob so the executable bit and symlink-ness are
/// preserved across the tree rebuild.
pub fn build_tree_from_oids(
    repo: &git2::Repository,
    base_tree: &git2::Tree<'_>,
    files: &[(PathBuf, git2::Oid, u32)],
) -> Result<git2::Oid, GitError> {
    build_tree_from_oids_with_deletions(repo, base_tree, files, &[])
}

/// Build a git tree by layering pre-created blob OIDs on top of `base_tree`,
/// and removing entries listed in `deletions`.
///
/// Deletions are specified as relative paths. Files are removed from the tree
/// recursively — if a deletion removes the last entry in a subdirectory, the
/// subdirectory is also removed.
pub fn build_tree_from_oids_with_deletions(
    repo: &git2::Repository,
    base_tree: &git2::Tree<'_>,
    files: &[(PathBuf, git2::Oid, u32)],
    deletions: &[PathBuf],
) -> Result<git2::Oid, GitError> {
    let mut root_blobs: Vec<(&std::ffi::OsStr, git2::Oid, u32)> = Vec::new();
    let mut subdir_files: HashMap<&std::ffi::OsStr, Vec<(PathBuf, git2::Oid, u32)>> =
        HashMap::new();

    for (path, oid, mode) in files {
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| GitError::MaterializationFailed("empty path".into()))?;

        let remaining: PathBuf = components.collect();
        let name = first.as_os_str();

        if remaining.as_os_str().is_empty() {
            root_blobs.push((name, *oid, *mode));
        } else {
            subdir_files
                .entry(name)
                .or_default()
                .push((remaining, *oid, *mode));
        }
    }

    // Partition deletions into root-level and subdirectory-level.
    let mut root_deletions: Vec<&std::ffi::OsStr> = Vec::new();
    let mut subdir_deletions: HashMap<&std::ffi::OsStr, Vec<PathBuf>> = HashMap::new();

    for path in deletions {
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| GitError::MaterializationFailed("empty deletion path".into()))?;

        let remaining: PathBuf = components.collect();
        let name = first.as_os_str();

        if remaining.as_os_str().is_empty() {
            root_deletions.push(name);
        } else {
            subdir_deletions.entry(name).or_default().push(remaining);
        }
    }

    let mut builder = repo.treebuilder(Some(base_tree))?;

    // Remove root-level deletions.
    for name in &root_deletions {
        let name_str = name.to_str().ok_or_else(|| {
            GitError::MaterializationFailed("non-UTF-8 filename in deletion".into())
        })?;
        // Ignore error if entry doesn't exist in tree.
        let _ = builder.remove(name_str);
    }

    for (name, oid, mode) in &root_blobs {
        builder.insert(
            name.to_str()
                .ok_or_else(|| GitError::MaterializationFailed("non-UTF-8 filename".into()))?,
            *oid,
            i32::try_from(*mode).map_err(|_| {
                GitError::MaterializationFailed(format!("invalid git mode {mode:o}"))
            })?,
        )?;
    }

    // Collect subdirectory names that need recursive processing (files or deletions).
    let mut subdir_names: std::collections::HashSet<&std::ffi::OsStr> =
        std::collections::HashSet::new();
    for name in subdir_files.keys() {
        subdir_names.insert(name);
    }
    for name in subdir_deletions.keys() {
        subdir_names.insert(name);
    }

    for dir_name in subdir_names {
        let dir_name_str = dir_name
            .to_str()
            .ok_or_else(|| GitError::MaterializationFailed("non-UTF-8 directory name".into()))?;

        let existing_subtree = base_tree
            .get_name(dir_name_str)
            .and_then(|entry| entry.to_object(repo).ok())
            .and_then(|obj| obj.into_tree().ok());

        let nested_files = subdir_files.get(dir_name).map_or(&[][..], |v| v.as_slice());
        let nested_deletions = subdir_deletions
            .get(dir_name)
            .map_or(&[][..], |v| v.as_slice());

        let subtree_oid = if let Some(ref sub) = existing_subtree {
            build_tree_from_oids_with_deletions(repo, sub, nested_files, nested_deletions)?
        } else if !nested_files.is_empty() {
            let empty_oid = repo.treebuilder(None)?.write()?;
            let empty_tree = repo.find_tree(empty_oid)?;
            build_tree_from_oids_with_deletions(repo, &empty_tree, nested_files, nested_deletions)?
        } else {
            // Only deletions, no existing subtree — nothing to do.
            continue;
        };

        // Check if the subtree is now empty after deletions; if so, remove the dir.
        let subtree = repo.find_tree(subtree_oid)?;
        if subtree.is_empty() {
            let _ = builder.remove(dir_name_str);
        } else {
            builder.insert(dir_name_str, subtree_oid, 0o040000)?;
        }
    }

    let tree_oid = builder.write()?;
    Ok(tree_oid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::init_repo;

    #[test]
    fn test_build_tree_from_oids_root_files() {
        let (dir, _ops) = init_repo(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]);
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = head.tree().unwrap();

        let file_oids = vec![
            (
                PathBuf::from("a.txt"),
                repo.blob(b"modified-a").unwrap(),
                0o100_644_u32,
            ),
            (
                PathBuf::from("c.txt"),
                repo.blob(b"new-c").unwrap(),
                0o100_644_u32,
            ),
        ];

        let new_tree_oid = build_tree_from_oids(&repo, &base_tree, &file_oids).unwrap();
        let new_tree = repo.find_tree(new_tree_oid).unwrap();

        let a_blob = repo
            .find_blob(new_tree.get_name("a.txt").unwrap().id())
            .unwrap();
        assert_eq!(a_blob.content(), b"modified-a");

        let b_blob = repo
            .find_blob(new_tree.get_name("b.txt").unwrap().id())
            .unwrap();
        assert_eq!(b_blob.content(), b"bbb");

        let c_blob = repo
            .find_blob(new_tree.get_name("c.txt").unwrap().id())
            .unwrap();
        assert_eq!(c_blob.content(), b"new-c");
    }

    #[test]
    fn test_build_tree_from_oids_nested_paths() {
        let (dir, _ops) = init_repo(&[
            ("src/main.rs", b"fn main() {}"),
            ("src/lib.rs", b"pub mod lib;"),
        ]);
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = head.tree().unwrap();

        let file_oids = vec![
            (
                PathBuf::from("src/main.rs"),
                repo.blob(b"fn main() { new }").unwrap(),
                0o100_644_u32,
            ),
            (
                PathBuf::from("src/utils/helper.rs"),
                repo.blob(b"pub fn help() {}").unwrap(),
                0o100_644_u32,
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
}
