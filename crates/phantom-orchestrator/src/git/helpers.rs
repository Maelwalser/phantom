//! Filesystem collection helpers and test-only git utilities.

use std::path::{Path, PathBuf};

/// Recursively collect all file paths relative to `root`.
pub(crate) fn collect_files_recursive(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
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

/// `commit_overlay_changes` physically copies overlay files into the working
/// tree and stages them via the index. This is intentionally **not** used in
/// production (see `Materializer::commit_from_oids` for the atomic,
/// OID-based approach). It is retained here for test helpers that need a quick
/// way to advance trunk without going through the full materializer pipeline.
#[cfg(test)]
impl super::GitOps {
    pub fn commit_overlay_changes(
        &self,
        upper_dir: &Path,
        trunk_path: &Path,
        message: &str,
        author: &str,
    ) -> Result<super::GitOid, crate::error::OrchestratorError> {
        use crate::error::OrchestratorError;

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

        Ok(super::oid_to_git_oid(new_oid))
    }
}
