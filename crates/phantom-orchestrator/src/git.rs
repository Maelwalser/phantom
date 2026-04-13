//! Git operations for Phantom, built on `git2`.
//!
//! Provides [`GitOps`] — a wrapper around a `git2::Repository` — and
//! lossless conversions between [`phantom_core::GitOid`] and [`git2::Oid`].

use std::path::{Path, PathBuf};

use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::id::{ChangesetId, GitOid};
use phantom_core::is_binary_or_non_utf8;
use phantom_core::traits::MergeResult;
use tracing::{debug, info};

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

    /// Commit all files from `upper_dir` into the repository whose working
    /// directory is `trunk_path`.
    ///
    /// 1. Recursively walk `upper_dir` to discover modified files.
    /// 2. Copy each file into the corresponding location under `trunk_path`.
    /// 3. Stage all changed files in the index.
    /// 4. Write the index as a tree and create a new commit.
    ///
    /// Returns the OID of the newly created commit.
    pub fn commit_overlay_changes(
        &self,
        upper_dir: &Path,
        trunk_path: &Path,
        message: &str,
        author: &str,
    ) -> Result<GitOid, OrchestratorError> {
        let files = collect_files_recursive(upper_dir)?;

        // Back up existing trunk files so we can restore on partial failure.
        let backup_dir = tempfile::TempDir::new().map_err(|e| {
            OrchestratorError::MaterializationFailed(format!(
                "failed to create backup directory: {e}"
            ))
        })?;
        let mut backed_up: Vec<PathBuf> = Vec::new();

        let copy_result = (|| -> Result<(), OrchestratorError> {
            for rel_path in &files {
                let src = upper_dir.join(rel_path);
                let dst = trunk_path.join(rel_path);

                // Back up the existing file if present.
                if dst.exists() || dst.symlink_metadata().is_ok() {
                    let backup_path = backup_dir.path().join(rel_path);
                    if let Some(parent) = backup_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let dst_meta = std::fs::symlink_metadata(&dst)?;
                    if dst_meta.is_symlink() {
                        let link_target = std::fs::read_link(&dst)?;
                        std::os::unix::fs::symlink(&link_target, &backup_path)?;
                    } else {
                        std::fs::copy(&dst, &backup_path)?;
                    }
                    backed_up.push(rel_path.clone());
                }

                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                let src_meta = std::fs::symlink_metadata(&src)?;
                if src_meta.is_symlink() {
                    let target = std::fs::read_link(&src)?;
                    let _ = std::fs::remove_file(&dst);
                    std::os::unix::fs::symlink(&target, &dst)?;
                } else {
                    std::fs::copy(&src, &dst)?;
                }
                debug!(path = %rel_path.display(), "copied overlay file to trunk");
            }
            Ok(())
        })();

        if let Err(e) = copy_result {
            // Restore backed-up files to leave trunk in its original state.
            for rel_path in &backed_up {
                let backup_path = backup_dir.path().join(rel_path);
                let dst = trunk_path.join(rel_path);
                let _ = std::fs::remove_file(&dst);
                if let Ok(meta) = std::fs::symlink_metadata(&backup_path) {
                    if meta.is_symlink() {
                        if let Ok(link_target) = std::fs::read_link(&backup_path) {
                            let _ = std::os::unix::fs::symlink(&link_target, &dst);
                        }
                    } else {
                        let _ = std::fs::copy(&backup_path, &dst);
                    }
                }
            }
            return Err(OrchestratorError::MaterializationFailed(format!(
                "overlay copy failed, trunk restored to original state: {e}"
            )));
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

        debug!(commit = %new_oid, "created commit");
        Ok(oid_to_git_oid(new_oid))
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
    if is_binary_or_non_utf8(base)
        || is_binary_or_non_utf8(ours)
        || is_binary_or_non_utf8(theirs)
    {
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
// Helpers
// ---------------------------------------------------------------------------

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
}
