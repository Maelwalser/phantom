//! Read-side operations for [`OverlayLayer`].
//!
//! All reads are driven by [`OverlayLayer::classify`]: the returned
//! [`ResolvedPath`] tells us whether to read from upper, lower, or passthrough.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::trace;

use crate::error::OverlayError;
use crate::trunk_view::read_dir_entries;
use crate::types::{DirEntry, is_hidden};
use crate::whiteout::{INTERNAL_FILES, is_safe_symlink_target};

use super::OverlayLayer;
use super::classify::ResolvedPath;

impl OverlayLayer {
    /// Read a file's contents. Checks upper layer first, then lower.
    ///
    /// Passthrough paths (`.git/**`) are read directly from the lower layer.
    /// Returns an error if the path has been whiteout'd (deleted) or is hidden.
    pub fn read_file(&self, rel_path: &Path) -> Result<Vec<u8>, OverlayError> {
        match self.classify(rel_path) {
            ResolvedPath::Upper(path) => {
                trace!(path = %rel_path.display(), layer = "upper", "reading file");
                Ok(fs::read(&path)?)
            }
            ResolvedPath::Lower(path) => {
                trace!(path = %rel_path.display(), layer = "lower", "reading file");
                Ok(fs::read(&path)?)
            }
            ResolvedPath::Passthrough(path) => {
                trace!(path = %rel_path.display(), layer = "lower-passthrough", "reading file");
                Ok(fs::read(&path)?)
            }
            ResolvedPath::NotFound => Err(OverlayError::PathNotFound(rel_path.to_path_buf())),
        }
    }

    /// Read the target of a symbolic link.
    ///
    /// Resolves the path through the overlay layers and reads the symlink
    /// target.  Targets that escape the overlay root (absolute paths or
    /// `..`-traversals that step outside) are not exposed — the call
    /// returns `PathNotFound` as if the link did not exist.  This prevents
    /// out-of-band writes to the upper layer from being reachable through
    /// the FUSE surface.
    pub fn read_symlink(&self, rel_path: &Path) -> Result<PathBuf, OverlayError> {
        match self.classify(rel_path) {
            ResolvedPath::Upper(path)
            | ResolvedPath::Lower(path)
            | ResolvedPath::Passthrough(path) => {
                let target = fs::read_link(&path)?;
                if !is_safe_symlink_target(&target, rel_path) {
                    return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
                }
                Ok(target)
            }
            ResolvedPath::NotFound => Err(OverlayError::PathNotFound(rel_path.to_path_buf())),
        }
    }

    /// Read a directory, merging entries from upper and lower layers.
    ///
    /// Passthrough directories read only from the lower layer. For normal
    /// directories, entries from both layers are merged (upper takes precedence),
    /// excluding whiteout'd and hidden entries. Passthrough entries (e.g. `.git`)
    /// are included when listing the root directory.
    pub fn read_dir(&self, rel_path: &Path) -> Result<Vec<DirEntry>, OverlayError> {
        // Passthrough directories read exclusively from lower.
        if crate::types::is_passthrough(rel_path) {
            let lower_dir = self.lower_path().join(rel_path);
            if lower_dir.is_dir() {
                return read_dir_entries(&lower_dir);
            }
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        // Snapshot whiteouts once for the duration of this listing.
        let whiteouts = self
            .whiteouts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let mut seen = HashSet::new();
        let mut entries = Vec::new();

        // Upper layer first (takes precedence).
        let upper_dir = self.upper.join(rel_path);
        if upper_dir.is_dir() {
            for entry in read_dir_entries(&upper_dir)? {
                let entry_rel = rel_path.join(&entry.name);
                if !whiteouts.contains(&entry_rel) && !is_hidden(&entry_rel) {
                    // Skip Phantom internal files.
                    let name_str = entry.name.to_string_lossy();
                    if !INTERNAL_FILES.iter().any(|f| name_str == *f) {
                        seen.insert(entry.name.clone());
                        entries.push(entry);
                    }
                }
            }
        }

        // Lower layer (only entries not already seen, not whiteout'd, not hidden).
        let lower_dir = self.lower_path().join(rel_path);
        if lower_dir.is_dir() {
            for entry in read_dir_entries(&lower_dir)? {
                let entry_rel = rel_path.join(&entry.name);
                if !seen.contains(&entry.name)
                    && !whiteouts.contains(&entry_rel)
                    && !is_hidden(&entry_rel)
                {
                    seen.insert(entry.name.clone());
                    entries.push(entry);
                }
            }
        }

        // Release the read lock before returning.
        drop(whiteouts);

        if entries.is_empty() && !upper_dir.is_dir() && !lower_dir.is_dir() {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        Ok(entries)
    }

    /// Check whether a path exists in the overlay.
    ///
    /// Passthrough paths check only the lower layer. Normal paths check upper
    /// then lower, excluding whiteout'd and hidden entries.
    #[must_use]
    pub fn exists(&self, rel_path: &Path) -> bool {
        !matches!(self.classify(rel_path), ResolvedPath::NotFound)
    }

    /// Get filesystem metadata for a path.
    ///
    /// Passthrough paths read metadata directly from the lower layer.
    /// Normal paths check upper first, then lower.
    pub fn getattr(&self, rel_path: &Path) -> Result<fs::Metadata, OverlayError> {
        match self.classify(rel_path) {
            ResolvedPath::Upper(path)
            | ResolvedPath::Lower(path)
            | ResolvedPath::Passthrough(path) => Ok(fs::symlink_metadata(&path)?),
            ResolvedPath::NotFound => Err(OverlayError::PathNotFound(rel_path.to_path_buf())),
        }
    }

    /// Resolve a relative path to its absolute on-disk location.
    ///
    /// Performs the same hidden/passthrough/whiteout/upper-vs-lower resolution
    /// as [`read_file`](Self::read_file), but returns the filesystem path
    /// instead of reading content. Used by the FUSE layer to open real file
    /// descriptors.
    pub fn resolve_path(&self, rel_path: &Path) -> Result<PathBuf, OverlayError> {
        match self.classify(rel_path) {
            ResolvedPath::Upper(path)
            | ResolvedPath::Lower(path)
            | ResolvedPath::Passthrough(path) => Ok(path),
            ResolvedPath::NotFound => Err(OverlayError::PathNotFound(rel_path.to_path_buf())),
        }
    }
}
