//! Git operations for Phantom, built on `git2`.
//!
//! Provides [`GitOps`] — a wrapper around a `git2::Repository` — and
//! lossless conversions between [`phantom_core::GitOid`] and [`git2::Oid`].

use std::path::{Path, PathBuf};

use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::id::{ChangesetId, GitOid};
use phantom_core::is_binary_or_non_utf8;
use phantom_core::traits::MergeResult;
use tracing::info;

use crate::error::OrchestratorError;

// ---------------------------------------------------------------------------
// GitOid ⇄ git2::Oid conversions
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
    repo: git2::Repository,
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

    /// Read the contents of `path` as it existed in the commit identified by `oid`.
    pub fn read_file_at_commit(
        &self,
        oid: &GitOid,
        path: &Path,
    ) -> Result<Vec<u8>, OrchestratorError> {
        let git_oid = git_oid_to_oid(oid)?;
        let commit = self.repo.find_commit(git_oid)?;
        let tree = commit.tree()?;

        let entry = tree.get_path(path).map_err(|_| {
            OrchestratorError::NotFound(format!("path not found in commit: {}", path.display()))
        })?;

        let blob = self.repo.find_blob(entry.id()).map_err(|_| {
            OrchestratorError::NotFound(format!("object at {} is not a blob", path.display()))
        })?;

        Ok(blob.content().to_vec())
    }

    /// List every blob path in the tree of the commit identified by `oid`.
    pub fn list_files_at_commit(&self, oid: &GitOid) -> Result<Vec<PathBuf>, OrchestratorError> {
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

    /// Revert a specific commit by creating a new commit that undoes its
    /// changes.
    ///
    /// This is the inverse of cherry-pick: it computes what the tree would look
    /// like if the given commit had never been applied, then commits that tree
    /// on top of the current HEAD. Subsequent commits are preserved.
    ///
    /// Returns the OID of the newly created revert commit, or an error if the
    /// revert produces conflicts (e.g. the changes were modified by a later
    /// commit).
    pub fn revert_commit_oid(
        &self,
        commit_oid: &GitOid,
        message: &str,
    ) -> Result<GitOid, OrchestratorError> {
        let git_oid = git_oid_to_oid(commit_oid)?;
        let revert_commit = self.repo.find_commit(git_oid)?;

        let head_oid_val = self.head_oid()?;
        let head_git_oid = git_oid_to_oid(&head_oid_val)?;
        let our_commit = self.repo.find_commit(head_git_oid)?;

        // mainline = 0 for non-merge commits
        let mut index = self
            .repo
            .revert_commit(&revert_commit, &our_commit, 0, None)?;

        if index.has_conflicts() {
            return Err(OrchestratorError::MaterializationFailed(
                "revert produced conflicts — the rolled-back changes were modified by a later commit".into(),
            ));
        }

        let tree_oid = index.write_tree_to(&self.repo)?;
        let tree = self.repo.find_tree(tree_oid)?;
        let sig = git2::Signature::now("phantom", "phantom@rollback")?;

        let new_oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&our_commit])?;

        // Update working directory to match
        self.repo
            .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))?;

        info!(reverted = %commit_oid, new_commit = %new_oid, "reverted commit");
        Ok(oid_to_git_oid(new_oid))
    }

    /// Hard-reset: move `HEAD` to `oid` and update the index and working tree
    /// to match.
    pub fn reset_to_commit(&self, oid: &GitOid) -> Result<(), OrchestratorError> {
        let git_oid = git_oid_to_oid(oid)?;
        let commit = self.repo.find_commit(git_oid)?;
        let obj = commit.as_object();
        self.repo.reset(obj, git2::ResetType::Hard, None)?;
        Ok(())
    }

    /// Return the list of file paths that differ between two commits.
    pub fn changed_files(
        &self,
        from: &GitOid,
        to: &GitOid,
    ) -> Result<Vec<PathBuf>, OrchestratorError> {
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

    /// Perform a line-based three-way merge.
    ///
    /// Returns [`MergeResult::Clean`] with the merged bytes on success, or
    /// [`MergeResult::Conflict`] with a [`ConflictDetail`] if the same region
    /// was modified on both sides.
    pub fn text_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
    ) -> Result<MergeResult, OrchestratorError> {
        three_way_merge(base, ours, theirs)
    }
}

// ---------------------------------------------------------------------------
// LCS-based three-way merge (via diffy)
// ---------------------------------------------------------------------------

/// Three-way merge using LCS-based diff alignment.
///
/// Computes the diff between base→ours and base→theirs independently,
/// then merges the changes. Correctly handles insertions and deletions
/// at arbitrary positions.
fn three_way_merge(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
) -> Result<MergeResult, OrchestratorError> {
    // Reject binary or non-UTF-8 content to prevent silent data corruption.
    if is_binary_or_non_utf8(base) || is_binary_or_non_utf8(ours) || is_binary_or_non_utf8(theirs) {
        let detail = ConflictDetail {
            kind: ConflictKind::BinaryFile,
            file: PathBuf::from("<text-merge>"),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "file is binary or not valid UTF-8; cannot text-merge".into(),
            ours_span: None,
            theirs_span: None,
            base_span: None,
        };
        return Ok(MergeResult::Conflict(vec![detail]));
    }

    // All three buffers were validated above, but use proper error propagation
    // rather than unwrap to stay robust against future changes in the guard.
    let base_s = std::str::from_utf8(base).map_err(|e| {
        OrchestratorError::MaterializationFailed(format!("base is not valid UTF-8: {e}"))
    })?;
    let ours_s = std::str::from_utf8(ours).map_err(|e| {
        OrchestratorError::MaterializationFailed(format!("ours is not valid UTF-8: {e}"))
    })?;
    let theirs_s = std::str::from_utf8(theirs).map_err(|e| {
        OrchestratorError::MaterializationFailed(format!("theirs is not valid UTF-8: {e}"))
    })?;

    let result = diffy::merge(base_s, ours_s, theirs_s);
    match result {
        Ok(merged) => Ok(MergeResult::Clean(merged.into_bytes())),
        Err(conflict_text) => {
            // diffy returns the conflicted text with markers.
            // We report this as a conflict.
            let detail = ConflictDetail {
                kind: ConflictKind::RawTextConflict,
                file: PathBuf::from("<text-merge>"),
                symbol_id: None,
                ours_changeset: ChangesetId("unknown".into()),
                theirs_changeset: ChangesetId("unknown".into()),
                description: "line-based three-way merge produced conflicts".into(),
                ours_span: None,
                theirs_span: None,
                base_span: None,
            };
            let _ = conflict_text; // conflict markers available if needed
            Ok(MergeResult::Conflict(vec![detail]))
        }
    }
}

// ---------------------------------------------------------------------------
// In-memory tree building
// ---------------------------------------------------------------------------

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
    use std::collections::HashMap;

    // Group files by their first path component. Files at the root level are
    // stored under None; files in subdirectories under Some(dir_name).
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
            // Leaf file at this level
            root_blobs.push((name, content));
        } else {
            // File inside a subdirectory
            subdir_files
                .entry(name)
                .or_default()
                .push((remaining, content));
        }
    }

    let mut builder = repo.treebuilder(Some(base_tree))?;

    // Insert leaf blobs
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

    // Recursively build subtrees
    for (dir_name, nested_files) in &subdir_files {
        let dir_name_str = dir_name.to_str().ok_or_else(|| {
            OrchestratorError::MaterializationFailed("non-UTF-8 directory name".into())
        })?;

        // Get existing subtree or create empty one
        let existing_subtree = base_tree
            .get_name(dir_name_str)
            .and_then(|entry| entry.to_object(repo).ok())
            .and_then(|obj| obj.into_tree().ok());

        // Convert nested_files to owned Vec for the recursive call
        let owned_files: Vec<(PathBuf, Vec<u8>)> = nested_files
            .iter()
            .map(|(p, c)| (p.clone(), c.to_vec()))
            .collect();

        let subtree_oid = if let Some(ref sub) = existing_subtree {
            build_tree_with_blobs(repo, sub, &owned_files)?
        } else {
            // No existing subtree — create from an empty tree
            let empty_oid = repo.treebuilder(None)?.write()?;
            let empty_tree = repo.find_tree(empty_oid)?;
            build_tree_with_blobs(repo, &empty_tree, &owned_files)?
        };

        builder.insert(dir_name_str, subtree_oid, 0o040000)?;
    }

    let tree_oid = builder.write()?;
    Ok(tree_oid)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// OID-based tree building (streaming-friendly)
// ---------------------------------------------------------------------------

/// Build a git tree by layering pre-created blob OIDs on top of `base_tree`.
///
/// This is the memory-efficient counterpart to [`build_tree_with_blobs`]:
/// callers create blobs up-front (one file at a time), then pass lightweight
/// `(PathBuf, git2::Oid)` pairs here.  The recursive calls only clone the
/// path suffix and copy the 20-byte OID — no file-content duplication.
pub fn build_tree_from_oids(
    repo: &git2::Repository,
    base_tree: &git2::Tree<'_>,
    files: &[(PathBuf, git2::Oid)],
) -> Result<git2::Oid, OrchestratorError> {
    use std::collections::HashMap;

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
/// `(path, blob_oid)` pairs.  Peak memory is one file's content at a time,
/// rather than all files simultaneously.
pub(crate) fn create_blobs_from_overlay(
    repo: &git2::Repository,
    upper_dir: &Path,
) -> Result<Vec<(PathBuf, git2::Oid)>, OrchestratorError> {
    let paths = collect_files_recursive(upper_dir)?;
    let mut file_oids = Vec::with_capacity(paths.len());
    for rel_path in paths {
        let content = std::fs::read(upper_dir.join(&rel_path))?;
        let blob_oid = repo.blob(&content)?;
        // `content` is dropped here — only one file buffered at a time.
        file_oids.push((rel_path, blob_oid));
    }
    Ok(file_oids)
}

/// Convert in-memory `(path, content)` pairs into `(path, blob_oid)` pairs
/// by creating git blob objects.  Used by the merge path where content is
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

/// Recursively collect all file paths relative to `root`.
fn collect_files_recursive(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut result = Vec::new();
    collect_files_inner(root, &PathBuf::new(), &mut result)?;
    Ok(result)
}

fn collect_files_inner(
    base: &Path,
    prefix: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let rel = prefix.join(entry.file_name());
        if ft.is_dir() {
            collect_files_inner(&entry.path(), &rel, out)?;
        } else if ft.is_file() {
            out.push(rel);
        } else if ft.is_symlink() {
            match entry.path().metadata() {
                Ok(meta) if meta.is_dir() => {
                    collect_files_inner(&entry.path(), &rel, out)?;
                }
                _ => {
                    out.push(rel);
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Test-only helpers on GitOps
// ---------------------------------------------------------------------------

/// `commit_overlay_changes` physically copies overlay files into the working
/// tree and stages them via the index. This is intentionally **not** used in
/// production (see `Materializer::commit_from_oids` for the atomic,
/// OID-based approach). It is retained here for test helpers that need a quick
/// way to advance trunk without going through the full materializer pipeline.
#[cfg(test)]
impl GitOps {
    pub fn commit_overlay_changes(
        &self,
        upper_dir: &Path,
        trunk_path: &Path,
        message: &str,
        author: &str,
    ) -> Result<GitOid, OrchestratorError> {
        let files = collect_files_recursive(upper_dir)?;

        for rel_path in &files {
            let src = upper_dir.join(rel_path);
            let dst = trunk_path.join(rel_path);

            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dst)?;
        }

        let mut index = self.repo.index()?;
        for rel_path in &files {
            index.add_path(rel_path)?;
        }
        index.write()?;

        let tree_oid = index.write_tree()?;
        let tree = self.repo.find_tree(tree_oid)?;

        let sig = git2::Signature::now(author, &format!("{author}@phantom"))?;

        let parent_commit = match self.repo.head() {
            Ok(head) => {
                let oid = head
                    .target()
                    .ok_or_else(|| OrchestratorError::NotFound("HEAD has no target".into()))?;
                Some(self.repo.find_commit(oid)?)
            }
            Err(e) if e.code() == git2::ErrorCode::UnbornBranch => None,
            Err(e) => return Err(OrchestratorError::Git(e)),
        };

        let parents: Vec<&git2::Commit<'_>> = parent_commit.iter().collect();
        let new_oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;

        Ok(oid_to_git_oid(new_oid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a temporary git repository with an initial commit and return
    /// `(TempDir, GitOps)`.
    fn init_repo_with_commit(
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

        let ops = GitOps { repo };
        (dir, ops)
    }

    #[test]
    fn test_open_and_head_oid() {
        let (dir, ops) = init_repo_with_commit(&[("a.txt", b"hello")], "init");
        let oid = ops.head_oid().unwrap();
        assert_ne!(oid, GitOid::zero());

        // Re-open by path
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

    #[test]
    fn test_commit_overlay_changes() {
        let (_dir, ops) = init_repo_with_commit(&[("src/main.rs", b"fn main() {}")], "init");
        let old_oid = ops.head_oid().unwrap();

        // Create an upper directory simulating agent overlay writes
        let upper = tempfile::TempDir::new().unwrap();
        let upper_main = upper.path().join("src/main.rs");
        std::fs::create_dir_all(upper_main.parent().unwrap()).unwrap();
        std::fs::write(&upper_main, b"fn main() { println!(\"hi\"); }").unwrap();

        let upper_lib = upper.path().join("src/lib.rs");
        std::fs::write(&upper_lib, b"pub fn greet() {}").unwrap();

        let trunk_path = ops.repo.workdir().unwrap().to_path_buf();
        let new_oid = ops
            .commit_overlay_changes(upper.path(), &trunk_path, "overlay commit", "agent-a")
            .unwrap();

        assert_ne!(old_oid, new_oid);
        assert_eq!(ops.head_oid().unwrap(), new_oid);

        // Verify file contents at new commit
        let main_content = ops
            .read_file_at_commit(&new_oid, Path::new("src/main.rs"))
            .unwrap();
        assert_eq!(main_content, b"fn main() { println!(\"hi\"); }");

        let lib_content = ops
            .read_file_at_commit(&new_oid, Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(lib_content, b"pub fn greet() {}");
    }

    #[test]
    fn test_reset_to_commit() {
        let (_dir, ops) = init_repo_with_commit(&[("a.txt", b"v1")], "commit 1");
        let first_oid = ops.head_oid().unwrap();

        // Make two more commits
        let trunk = ops.repo.workdir().unwrap().to_path_buf();

        let upper2 = tempfile::TempDir::new().unwrap();
        std::fs::write(upper2.path().join("a.txt"), b"v2").unwrap();
        let second_oid = ops
            .commit_overlay_changes(upper2.path(), &trunk, "commit 2", "test")
            .unwrap();

        let upper3 = tempfile::TempDir::new().unwrap();
        std::fs::write(upper3.path().join("a.txt"), b"v3").unwrap();
        let _third_oid = ops
            .commit_overlay_changes(upper3.path(), &trunk, "commit 3", "test")
            .unwrap();

        assert_ne!(ops.head_oid().unwrap(), first_oid);

        // Reset to first commit
        ops.reset_to_commit(&first_oid).unwrap();
        assert_eq!(ops.head_oid().unwrap(), first_oid);

        // Reset to second commit
        ops.reset_to_commit(&second_oid).unwrap();
        assert_eq!(ops.head_oid().unwrap(), second_oid);
    }

    #[test]
    fn test_changed_files() {
        let (_dir, ops) = init_repo_with_commit(&[("a.txt", b"aaa"), ("b.txt", b"bbb")], "init");
        let first_oid = ops.head_oid().unwrap();

        // Commit a change to a.txt and add c.txt
        let trunk = ops.repo.workdir().unwrap().to_path_buf();
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
    fn test_text_merge_clean() {
        let (_dir, ops) = init_repo_with_commit(&[("x.txt", b"x")], "init");

        // Ours changes line 2, theirs changes line 4 — disjoint edits, clean merge.
        let base = b"a\nb\nc\nd\n";
        let ours = b"a\nB\nc\nd\n"; // changed b→B
        let theirs = b"a\nb\nc\nD\n"; // changed d→D

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8(merged).unwrap();
                assert!(text.contains('B'), "should contain ours' change");
                assert!(text.contains('D'), "should contain theirs' change");
            }
            MergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_text_merge_conflict() {
        let (_dir, ops) = init_repo_with_commit(&[("x.txt", b"x")], "init");

        let base = b"a\nb\nc\n";
        let ours = b"a\nX\nc\n";
        let theirs = b"a\nY\nc\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Clean(_) => panic!("expected conflict"),
            MergeResult::Conflict(details) => {
                assert!(!details.is_empty());
            }
        }
    }

    #[test]
    fn test_git_oid_roundtrip() {
        let hex = "aabbccddee00112233445566778899aabbccddee";
        let original = git2::Oid::from_str(hex).unwrap();

        let phantom_oid = oid_to_git_oid(original);
        let recovered = git_oid_to_oid(&phantom_oid).unwrap();

        assert_eq!(original, recovered);
    }

    #[test]
    fn test_text_merge_rejects_binary() {
        let (_dir, ops) = init_repo_with_commit(&[("a.bin", b"init")], "init");
        let base = b"some text\n";
        let ours = b"some\x00binary\n";
        let theirs = b"other text\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }

    #[test]
    fn test_text_merge_rejects_non_utf8() {
        let (_dir, ops) = init_repo_with_commit(&[("a.txt", b"init")], "init");
        let base = b"hello\n";
        let ours = b"hello\n";
        let theirs = b"\xff\xfe\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Conflict(conflicts) => {
                assert_eq!(conflicts[0].kind, ConflictKind::BinaryFile);
            }
            MergeResult::Clean(_) => panic!("expected BinaryFile conflict"),
        }
    }

    #[test]
    fn test_recovery_failure_reported() {
        // Verify MaterializationRecoveryFailed variant formats correctly and
        // distinguishes itself from the normal MaterializationFailed.
        let err = OrchestratorError::MaterializationRecoveryFailed {
            cause: "copy failed: disk full".into(),
            recovery_errors: "restore a.txt: permission denied".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("RECOVERY ALSO FAILED"), "error was: {msg}");
        assert!(msg.contains("disk full"), "error was: {msg}");
        assert!(msg.contains("permission denied"), "error was: {msg}");

        // Normal recovery success uses MaterializationFailed (no recovery error).
        let ok_err = OrchestratorError::MaterializationFailed(
            "overlay copy failed, trunk restored to original state: disk full".into(),
        );
        let ok_msg = ok_err.to_string();
        assert!(
            !ok_msg.contains("RECOVERY ALSO FAILED"),
            "error was: {ok_msg}"
        );
        assert!(
            ok_msg.contains("restored to original state"),
            "error was: {ok_msg}"
        );
    }

    #[test]
    fn test_build_tree_with_blobs_root_files() {
        let (dir, _ops) = init_repo_with_commit(&[("a.txt", b"aaa"), ("b.txt", b"bbb")], "init");
        let repo = git2::Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let base_tree = head.tree().unwrap();

        let files = vec![
            (PathBuf::from("a.txt"), b"modified-a".to_vec()),
            (PathBuf::from("c.txt"), b"new-c".to_vec()),
        ];

        let new_tree_oid = build_tree_with_blobs(&repo, &base_tree, &files).unwrap();
        let new_tree = repo.find_tree(new_tree_oid).unwrap();

        // a.txt should be modified
        let a_entry = new_tree.get_name("a.txt").unwrap();
        let a_blob = repo.find_blob(a_entry.id()).unwrap();
        assert_eq!(a_blob.content(), b"modified-a");

        // b.txt should be preserved from base
        let b_entry = new_tree.get_name("b.txt").unwrap();
        let b_blob = repo.find_blob(b_entry.id()).unwrap();
        assert_eq!(b_blob.content(), b"bbb");

        // c.txt should be newly added
        let c_entry = new_tree.get_name("c.txt").unwrap();
        let c_blob = repo.find_blob(c_entry.id()).unwrap();
        assert_eq!(c_blob.content(), b"new-c");
    }

    #[test]
    fn test_build_tree_with_blobs_nested_paths() {
        let (dir, _ops) = init_repo_with_commit(
            &[
                ("src/main.rs", b"fn main() {}"),
                ("src/lib.rs", b"pub mod lib;"),
            ],
            "init",
        );
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

        // Navigate into src/
        let src_entry = new_tree.get_name("src").unwrap();
        let src_tree = repo.find_tree(src_entry.id()).unwrap();

        // main.rs should be modified
        let main_entry = src_tree.get_name("main.rs").unwrap();
        let main_blob = repo.find_blob(main_entry.id()).unwrap();
        assert_eq!(main_blob.content(), b"fn main() { new }");

        // lib.rs should be preserved
        let lib_entry = src_tree.get_name("lib.rs").unwrap();
        let lib_blob = repo.find_blob(lib_entry.id()).unwrap();
        assert_eq!(lib_blob.content(), b"pub mod lib;");

        // utils/helper.rs should be newly created
        let utils_entry = src_tree.get_name("utils").unwrap();
        let utils_tree = repo.find_tree(utils_entry.id()).unwrap();
        let helper_entry = utils_tree.get_name("helper.rs").unwrap();
        let helper_blob = repo.find_blob(helper_entry.id()).unwrap();
        assert_eq!(helper_blob.content(), b"pub fn help() {}");
    }

    #[test]
    fn test_build_tree_from_oids_root_files() {
        let (dir, _ops) = init_repo_with_commit(&[("a.txt", b"aaa"), ("b.txt", b"bbb")], "init");
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
        let (dir, _ops) = init_repo_with_commit(
            &[
                ("src/main.rs", b"fn main() {}"),
                ("src/lib.rs", b"pub mod lib;"),
            ],
            "init",
        );
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
        let (dir, _ops) = init_repo_with_commit(&[("x.txt", b"x")], "init");
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
        let (dir, _ops) = init_repo_with_commit(&[("x.txt", b"x")], "init");
        let repo = git2::Repository::open(dir.path()).unwrap();

        // Create a temporary overlay upper directory with files
        let upper = tempfile::TempDir::new().unwrap();
        std::fs::write(upper.path().join("hello.txt"), b"hello").unwrap();
        std::fs::create_dir(upper.path().join("sub")).unwrap();
        std::fs::write(upper.path().join("sub/nested.txt"), b"nested").unwrap();

        let oids = create_blobs_from_overlay(&repo, upper.path()).unwrap();
        assert_eq!(oids.len(), 2);

        // Verify blobs were created correctly
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
