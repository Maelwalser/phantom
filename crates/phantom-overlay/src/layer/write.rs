//! Write-side operations for [`OverlayLayer`].
//!
//! All writes go to the upper (agent) layer except for passthrough paths,
//! which write directly to the lower layer. COW copies happen outside the
//! whiteout lock so slow I/O never blocks readers.

use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tracing::{debug, trace};

use crate::error::OverlayError;
use crate::types::{is_hidden, is_passthrough};
use crate::whiteout::{is_safe_relative_path, is_safe_symlink_target};

use super::OverlayLayer;
use super::io_util::ensure_parent_dir;

impl OverlayLayer {
    /// Write a file to the upper layer (or directly to lower for passthrough paths).
    ///
    /// Creates parent directories as needed. Automatically removes the path
    /// from the whiteout set if it was previously deleted.
    pub fn write_file(&self, rel_path: &Path, data: &[u8]) -> Result<(), OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }
        if !is_safe_relative_path(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }
        if is_passthrough(rel_path) {
            let lower_path = self.lower_path().join(rel_path);
            ensure_parent_dir(&lower_path)?;
            fs::write(&lower_path, data)?;
            trace!(path = %rel_path.display(), layer = "lower-passthrough", "writing file");
            return Ok(());
        }

        let upper_path = self.upper.join(rel_path);
        ensure_parent_dir(&upper_path)?;
        // I/O happens before acquiring the whiteout lock.
        fs::write(&upper_path, data)?;

        if self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path)
        {
            self.persist_whiteouts_or_warn();
        }

        Ok(())
    }

    /// Truncate or extend a file to `new_size` bytes without reading the full
    /// contents into memory.
    ///
    /// Uses `File::set_len` (`ftruncate(2)`) to adjust the file size in-place.
    /// When the file only exists in the lower layer, it is first copied to the
    /// upper layer (only up to `new_size` bytes for truncation, avoiding a full
    /// read of large files).
    pub fn truncate_file(&self, rel_path: &Path, new_size: u64) -> Result<(), OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }
        if !is_safe_relative_path(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }
        if is_passthrough(rel_path) {
            let lower_path = self.lower_path().join(rel_path);
            let file = OpenOptions::new().write(true).open(&lower_path)?;
            file.set_len(new_size)?;
            trace!(path = %rel_path.display(), new_size, layer = "lower-passthrough", "truncate");
            return Ok(());
        }

        let upper_path = self.upper.join(rel_path);

        if upper_path.exists() {
            // File already in upper layer — truncate in place.
            let file = OpenOptions::new().write(true).open(&upper_path)?;
            file.set_len(new_size)?;
        } else {
            // File only exists in the lower layer — COW copy to upper before truncating.
            let lower_path = self.lower_path().join(rel_path);
            let is_whiteout = self
                .whiteouts
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(rel_path);
            if !lower_path.exists() && !is_whiteout {
                return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
            }

            ensure_parent_dir(&upper_path)?;

            // COW copy happens outside any lock.
            if lower_path.exists() {
                let lower_size = fs::metadata(&lower_path)?.len();
                let copy_bytes = new_size.min(lower_size);

                let src = fs::File::open(&lower_path)?;
                let mut dst = fs::File::create(&upper_path)?;

                if copy_bytes > 0 {
                    std::io::copy(&mut src.take(copy_bytes), &mut dst)?;
                }
                dst.set_len(new_size)?;
            } else {
                // Whiteout'd file being re-created via truncate (rare but valid).
                let file = fs::File::create(&upper_path)?;
                file.set_len(new_size)?;
            }
        }

        if self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path)
        {
            self.persist_whiteouts_or_warn();
        }

        trace!(path = %rel_path.display(), new_size, layer = "upper", "truncate");
        Ok(())
    }

    /// Set file permissions in the overlay.
    ///
    /// Passthrough paths are modified directly in the lower layer.
    /// For normal paths, if the file only exists in the lower layer it is first
    /// copied to the upper layer (COW) before applying the permission change.
    pub fn set_permissions(&self, rel_path: &Path, mode: u32) -> Result<(), OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let perms = fs::Permissions::from_mode(mode);

        if is_passthrough(rel_path) {
            let lower_path = self.lower_path().join(rel_path);
            if !lower_path.exists() {
                return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
            }
            fs::set_permissions(&lower_path, perms)?;
            trace!(path = %rel_path.display(), mode = format!("{mode:#o}"), layer = "lower-passthrough", "chmod");
            return Ok(());
        }

        if self
            .whiteouts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(rel_path)
        {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let upper_path = self.upper.join(rel_path);

        if !upper_path.exists() {
            // COW: copy from lower to upper before changing permissions.
            // I/O happens outside any lock.
            let lower_path = self.lower_path().join(rel_path);
            if !lower_path.exists() {
                return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
            }

            ensure_parent_dir(&upper_path)?;

            fs::copy(&lower_path, &upper_path)?;
        }

        fs::set_permissions(&upper_path, perms)?;
        trace!(path = %rel_path.display(), mode = format!("{mode:#o}"), layer = "upper", "chmod");
        Ok(())
    }

    /// Create a symbolic link in the overlay.
    ///
    /// Passthrough paths create the symlink directly in the lower layer.
    /// For normal paths, the symlink is created in the upper layer.
    /// Automatically removes the path from the whiteout set if it was
    /// previously deleted.
    pub fn create_symlink(&self, rel_path: &Path, target: &Path) -> Result<(), OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }
        // Refuse symlinks whose target escapes the overlay root.  Absolute
        // targets and `..`-traversals that step outside are rejected so one
        // agent cannot plant a link that points into another agent's overlay
        // or a system path.
        if !is_safe_symlink_target(target, rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if is_passthrough(rel_path) {
            let lower_path = self.lower_path().join(rel_path);
            ensure_parent_dir(&lower_path)?;
            std::os::unix::fs::symlink(target, &lower_path)?;
            trace!(path = %rel_path.display(), target = %target.display(), layer = "lower-passthrough", "symlink");
            return Ok(());
        }

        let upper_path = self.upper.join(rel_path);
        ensure_parent_dir(&upper_path)?;
        std::os::unix::fs::symlink(target, &upper_path)?;

        if self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path)
        {
            self.persist_whiteouts_or_warn();
        }

        trace!(path = %rel_path.display(), target = %target.display(), layer = "upper", "symlink");
        Ok(())
    }

    /// Delete a file from the overlay.
    ///
    /// Passthrough paths are deleted directly from the lower layer (no whiteout).
    /// For normal paths, if the file exists in the upper layer it is removed, and
    /// the path is added to the whiteout set so the lower layer's version is hidden.
    pub fn delete_file(&self, rel_path: &Path) -> Result<(), OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }
        if !is_safe_relative_path(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }
        if is_passthrough(rel_path) {
            let lower_path = self.lower_path().join(rel_path);
            if lower_path.exists() {
                fs::remove_file(&lower_path)?;
            }
            debug!(path = %rel_path.display(), "passthrough file deleted");
            return Ok(());
        }

        let upper_path = self.upper.join(rel_path);
        let lower = self.lower_path();
        let lower_path = lower.join(rel_path);
        let upper_exists = fs::symlink_metadata(&upper_path).is_ok();
        let lower_exists = fs::symlink_metadata(&lower_path).is_ok();

        if !upper_exists
            && !lower_exists
            && !self
                .whiteouts
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(rel_path)
        {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        // I/O happens before acquiring the whiteout lock.
        if upper_exists {
            fs::remove_file(&upper_path)?;
        }

        self.whiteouts
            .write()
            .unwrap()
            .insert(rel_path.to_path_buf());
        self.persist_whiteouts()?;

        debug!(path = %rel_path.display(), "file deleted (whiteout created)");
        Ok(())
    }

    /// Ensure a file exists in the upper layer, performing a streaming COW copy
    /// from the lower layer if necessary.
    ///
    /// Returns the absolute path to the file in the upper layer. For passthrough
    /// paths, returns the lower-layer path directly (writes go there).
    ///
    /// Used by the FUSE layer on writable `open()` so that subsequent
    /// `pwrite(2)` calls can operate directly on the upper-layer file.
    pub fn ensure_upper_copy(&self, rel_path: &Path) -> Result<std::path::PathBuf, OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if is_passthrough(rel_path) {
            let lower_path = self.lower_path().join(rel_path);
            if lower_path.exists() {
                return Ok(lower_path);
            }
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let upper_path = self.upper.join(rel_path);

        if upper_path.exists() {
            // Already in the upper layer.
            if self
                .whiteouts
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(rel_path)
            {
                self.persist_whiteouts_or_warn();
            }
            return Ok(upper_path);
        }

        // Not in upper — check lower for COW copy.
        let lower_path = self.lower_path().join(rel_path);
        let is_whiteout = self
            .whiteouts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(rel_path);
        if !lower_path.exists() && !is_whiteout {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        ensure_parent_dir(&upper_path)?;

        // Streaming COW copy happens OUTSIDE any lock — this is the key
        // performance improvement over the old global RwLock<OverlayLayer>.
        if lower_path.exists() {
            let mut src = fs::File::open(&lower_path)?;
            let mut dst = fs::File::create(&upper_path)?;
            std::io::copy(&mut src, &mut dst)?;
            trace!(path = %rel_path.display(), "COW copy to upper layer");
        } else {
            // Whiteout'd file being re-opened for write — create empty.
            fs::File::create(&upper_path)?;
        }

        // Brief write lock to update whiteout set after I/O is done.
        if self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path)
        {
            self.persist_whiteouts_or_warn();
        }

        Ok(upper_path)
    }
}
