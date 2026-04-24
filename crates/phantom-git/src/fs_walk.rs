//! Filesystem collection helpers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Maximum directory nesting to walk before bailing. Protects against
/// pathologically deep trees and any symlink cycle the inode check fails
/// to catch (e.g. across filesystem boundaries where `dev` differs).
const MAX_DEPTH: usize = 64;

/// Recursively collect all file paths relative to `root`.
pub(crate) fn collect_files_recursive(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut result = Vec::new();
    let mut visited: HashSet<(u64, u64)> = HashSet::new();
    collect_files_inner(root, &PathBuf::new(), &mut result, &mut visited, 0)?;
    Ok(result)
}

fn collect_files_inner(
    base: &Path,
    prefix: &Path,
    out: &mut Vec<PathBuf>,
    visited: &mut HashSet<(u64, u64)>,
    depth: usize,
) -> Result<(), std::io::Error> {
    if depth > MAX_DEPTH {
        tracing::warn!(
            path = %base.display(),
            depth,
            "fs_walk depth limit exceeded; skipping deeper entries"
        );
        return Ok(());
    }

    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let rel = prefix.join(entry.file_name());
        if ft.is_dir() {
            if let Some(key) = dev_ino(&entry.path())
                && !visited.insert(key)
            {
                // Cycle already visited via another path; skip to avoid
                // double-emitting its contents.
                continue;
            }
            collect_files_inner(&entry.path(), &rel, out, visited, depth + 1)?;
        } else if ft.is_file() {
            out.push(rel);
        } else if ft.is_symlink() {
            // Only follow symlinks that resolve to directories; otherwise
            // record the link itself. Track visited (dev, ino) pairs so a
            // circular symlink (e.g. `upper/link -> upper/`) does not
            // blow the stack.
            match entry.path().metadata() {
                Ok(meta) if meta.is_dir() => {
                    if let Some(key) = dev_ino(&entry.path())
                        && !visited.insert(key)
                    {
                        tracing::warn!(
                            path = %entry.path().display(),
                            "symlink cycle detected; not re-entering"
                        );
                        continue;
                    }
                    collect_files_inner(&entry.path(), &rel, out, visited, depth + 1)?;
                }
                _ => {
                    out.push(rel);
                }
            }
        }
    }
    Ok(())
}

/// Return the `(dev, ino)` pair for a path, used as a cycle-detection key.
/// Falls back to `None` on any stat failure rather than panicking; the
/// caller then treats that branch as safe-to-recurse.
fn dev_ino(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
