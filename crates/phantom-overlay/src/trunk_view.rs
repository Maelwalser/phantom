//! Read-only view of the trunk working tree.
//!
//! [`TrunkView`] provides read access to files and directories in the
//! underlying git working tree. It is used as the lower layer of the
//! copy-on-write overlay.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::OverlayError;
use crate::layer::DirEntry;

/// Read-only view into the trunk working tree.
pub struct TrunkView {
    work_tree: PathBuf,
}

impl TrunkView {
    /// Create a new trunk view rooted at the given working tree path.
    #[must_use]
    pub fn new(work_tree: PathBuf) -> Self {
        Self { work_tree }
    }

    /// Read file contents at a path relative to the work tree root.
    pub fn read_file(&self, rel_path: &Path) -> Result<Vec<u8>, OverlayError> {
        let full = self.work_tree.join(rel_path);
        fs::read(&full).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                OverlayError::PathNotFound(rel_path.to_path_buf())
            } else {
                OverlayError::Io(e)
            }
        })
    }

    /// List directory entries at a path relative to the work tree root.
    pub fn list_dir(&self, rel_path: &Path) -> Result<Vec<DirEntry>, OverlayError> {
        let full = self.work_tree.join(rel_path);
        read_dir_entries(&full)
    }

    /// Get file metadata at a path relative to the work tree root.
    pub fn file_attr(&self, rel_path: &Path) -> Result<fs::Metadata, OverlayError> {
        let full = self.work_tree.join(rel_path);
        fs::metadata(&full).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                OverlayError::PathNotFound(rel_path.to_path_buf())
            } else {
                OverlayError::Io(e)
            }
        })
    }

    /// Return the work tree root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.work_tree
    }
}

/// Read directory entries from an absolute path, converting to [`DirEntry`].
pub(crate) fn read_dir_entries(abs_path: &Path) -> Result<Vec<DirEntry>, OverlayError> {
    let rd = fs::read_dir(abs_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            OverlayError::PathNotFound(abs_path.to_path_buf())
        } else {
            OverlayError::Io(e)
        }
    })?;

    let mut entries = Vec::new();
    for entry in rd {
        let entry = entry?;
        let ft = entry.file_type()?;
        let file_type = if ft.is_dir() {
            crate::layer::FileType::Directory
        } else if ft.is_symlink() {
            crate::layer::FileType::Symlink
        } else {
            crate::layer::FileType::File
        };
        entries.push(DirEntry {
            name: entry.file_name(),
            file_type,
        });
    }
    Ok(entries)
}

/// Recursively collect all file paths under `dir`, returning paths relative to `base`.
pub(crate) fn walk_files(dir: &Path, base: &Path) -> Result<Vec<PathBuf>, OverlayError> {
    let mut result = Vec::new();
    if !dir.is_dir() {
        return Ok(result);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            result.extend(walk_files(&path, base)?);
        } else {
            if let Ok(rel) = path.strip_prefix(base) {
                result.push(rel.to_path_buf());
            }
        }
    }
    Ok(result)
}

