//! Git operations for Phantom, built on `git2`.
//!
//! Provides [`GitOps`] — a wrapper around a `git2::Repository` — and
//! lossless conversions between [`phantom_core::GitOid`] and [`git2::Oid`].

use std::path::{Path, PathBuf};

use phantom_core::conflict::{ConflictDetail, ConflictKind};
use phantom_core::id::{ChangesetId, GitOid};
use phantom_core::traits::MergeResult;
use tracing::debug;

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
            OrchestratorError::NotFound(format!(
                "object at {} is not a blob",
                path.display()
            ))
        })?;

        Ok(blob.content().to_vec())
    }

    /// List every blob path in the tree of the commit identified by `oid`.
    pub fn list_files_at_commit(
        &self,
        oid: &GitOid,
    ) -> Result<Vec<PathBuf>, OrchestratorError> {
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

        for rel_path in &files {
            let src = upper_dir.join(rel_path);
            let dst = trunk_path.join(rel_path);

            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dst)?;
            debug!(path = %rel_path.display(), "copied overlay file to trunk");
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
        let new_oid = self.repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)?;

        debug!(commit = %new_oid, "created commit");
        Ok(oid_to_git_oid(new_oid))
    }

    /// Hard-reset: move `HEAD` to `oid` and update the index and working tree
    /// to match.
    pub fn reset_to_commit(&self, oid: &GitOid) -> Result<(), OrchestratorError> {
        let git_oid = git_oid_to_oid(oid)?;
        let commit = self.repo.find_commit(git_oid)?;
        let obj = commit.as_object();
        self.repo
            .reset(obj, git2::ResetType::Hard, None)?;
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
// Line-based three-way merge
// ---------------------------------------------------------------------------

/// Simple line-based three-way merge.
///
/// Compares each line of `ours` and `theirs` against `base`. When both sides
/// change the same line to different values, the merge is conflicted.
fn three_way_merge(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
) -> Result<MergeResult, OrchestratorError> {
    let base_s = String::from_utf8_lossy(base);
    let ours_s = String::from_utf8_lossy(ours);
    let theirs_s = String::from_utf8_lossy(theirs);

    let base_lines: Vec<&str> = base_s.lines().collect();
    let ours_lines: Vec<&str> = ours_s.lines().collect();
    let theirs_lines: Vec<&str> = theirs_s.lines().collect();

    let max_len = base_lines
        .len()
        .max(ours_lines.len())
        .max(theirs_lines.len());

    let mut merged = Vec::with_capacity(max_len);
    let mut has_conflict = false;

    for i in 0..max_len {
        let b = base_lines.get(i).copied();
        let o = ours_lines.get(i).copied();
        let t = theirs_lines.get(i).copied();

        match (b, o, t) {
            // All three agree
            (Some(bl), Some(ol), Some(tl)) if ol == bl && tl == bl => {
                merged.push(bl.to_string());
            }
            // Only ours changed
            (Some(bl), Some(ol), Some(tl)) if tl == bl => {
                merged.push(ol.to_string());
            }
            // Only theirs changed
            (Some(bl), Some(ol), Some(tl)) if ol == bl => {
                merged.push(tl.to_string());
            }
            // Both changed to the same thing
            (Some(_), Some(ol), Some(tl)) if ol == tl => {
                merged.push(ol.to_string());
            }
            // Both changed to different things — conflict
            (Some(_), Some(_), Some(_)) => {
                has_conflict = true;
                break;
            }
            // Lines beyond base — only ours has extra
            (None, Some(ol), None) => {
                merged.push(ol.to_string());
            }
            // Lines beyond base — only theirs has extra
            (None, None, Some(tl)) => {
                merged.push(tl.to_string());
            }
            // Both added same line beyond base
            (None, Some(ol), Some(tl)) if ol == tl => {
                merged.push(ol.to_string());
            }
            // Both added different lines beyond base — conflict
            (None, Some(_), Some(_)) => {
                has_conflict = true;
                break;
            }
            // Line deleted by ours (base had it, ours doesn't, theirs keeps it)
            (Some(bl), None, Some(tl)) if tl == bl => {
                // ours deleted this line — skip it
            }
            // Line deleted by theirs
            (Some(bl), Some(ol), None) if ol == bl => {
                // theirs deleted this line — skip it
            }
            // Modify + delete — conflict
            (Some(_), None, Some(_)) | (Some(_), Some(_), None) => {
                has_conflict = true;
                break;
            }
            // Both deleted
            (Some(_), None, None) => {
                // both deleted — skip
            }
            // Only base is None (both added same position) — handled above
            // Remaining: all None
            (None, None, None) => {}
        }
    }

    if has_conflict {
        let detail = ConflictDetail {
            kind: ConflictKind::RawTextConflict,
            file: PathBuf::from("<text-merge>"),
            symbol_id: None,
            ours_changeset: ChangesetId("unknown".into()),
            theirs_changeset: ChangesetId("unknown".into()),
            description: "line-based three-way merge produced conflicts".into(),
        };
        Ok(MergeResult::Conflict(vec![detail]))
    } else {
        let mut result = merged.join("\n");
        // Preserve trailing newline if base had one
        if base.last() == Some(&b'\n') {
            result.push('\n');
        }
        Ok(MergeResult::Clean(result.into_bytes()))
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
        let (_dir, ops) =
            init_repo_with_commit(&[("src/main.rs", b"fn main() {}")], "init");
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
        let (_dir, ops) = init_repo_with_commit(
            &[("a.txt", b"aaa"), ("b.txt", b"bbb")],
            "init",
        );
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

        assert_eq!(changed, vec![PathBuf::from("a.txt"), PathBuf::from("c.txt")]);
    }

    #[test]
    fn test_text_merge_clean() {
        let (_dir, ops) = init_repo_with_commit(&[("x.txt", b"x")], "init");

        let base = b"a\nb\nc\n";
        let ours = b"a\nb\nd\n";
        let theirs = b"a\ne\nc\n";

        let result = ops.text_merge(base, ours, theirs).unwrap();
        match result {
            MergeResult::Clean(merged) => {
                let text = String::from_utf8(merged).unwrap();
                assert!(text.contains('e'), "should contain theirs' change");
                assert!(text.contains('d'), "should contain ours' change");
                assert!(!text.contains('b'), "base-only line 'b' should be gone");
                assert!(!text.contains('c'), "base-only line 'c' should be gone");
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
}
