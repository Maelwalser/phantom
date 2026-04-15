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

