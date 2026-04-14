//! Git tree building from blobs and overlay files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::OrchestratorError;

use super::helpers::collect_files_recursive;

/// Build a new tree by layering `files` (with in-memory content) on top of
/// `base_tree`.
///
/// **Deprecated:** Prefer [`build_tree_from_oids`] with pre-created blobs to
/// avoid duplicating file content at each recursion level.
#[cfg(test)]
pub fn build_tree_with_blobs(
    repo: &git2::Repository,
    base_tree: &git2::Tree<'_>,
    files: &[(PathBuf, Vec<u8>)],
) -> Result<git2::Oid, OrchestratorError> {
    let mut root_blobs: Vec<(&std::ffi::OsStr, &[u8])> = Vec::new();
    let mut subdir_files: HashMap<&std::ffi::OsStr, Vec<(PathBuf, &[u8])>> = HashMap::new();

    for (path, content) in files {
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| OrchestratorError::MaterializationFailed("empty path".into()))?;

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
                OrchestratorError::MaterializationFailed("non-UTF-8 filename".into())
            })?,
            blob_oid,
            0o100644,
        )?;
    }

    for (dir_name, nested_files) in &subdir_files {
        let dir_name_str = dir_name.to_str().ok_or_else(|| {
            OrchestratorError::MaterializationFailed("non-UTF-8 directory name".into())
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
) -> Result<git2::Oid, OrchestratorError> {
    let mut root_blobs: Vec<(&std::ffi::OsStr, git2::Oid)> = Vec::new();
    let mut subdir_files: HashMap<&std::ffi::OsStr, Vec<(PathBuf, git2::Oid)>> = HashMap::new();

    for (path, oid) in files {
        let mut components = path.components();
        let first = components
            .next()
            .ok_or_else(|| OrchestratorError::MaterializationFailed("empty path".into()))?;

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
                OrchestratorError::MaterializationFailed("non-UTF-8 filename".into())
            })?,
            *oid,
            0o100644,
        )?;
    }

    for (dir_name, nested_files) in &subdir_files {
        let dir_name_str = dir_name.to_str().ok_or_else(|| {
            OrchestratorError::MaterializationFailed("non-UTF-8 directory name".into())
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
pub(crate) fn create_blobs_from_overlay(
    repo: &git2::Repository,
    upper_dir: &Path,
) -> Result<Vec<(PathBuf, git2::Oid)>, OrchestratorError> {
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
) -> Result<Vec<(PathBuf, git2::Oid)>, OrchestratorError> {
    let mut file_oids = Vec::with_capacity(files.len());
    for (path, content) in files {
        let blob_oid = repo.blob(content)?;
        file_oids.push((path.clone(), blob_oid));
    }
    Ok(file_oids)
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
