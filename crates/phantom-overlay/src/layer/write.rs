//! Write-side operations for [`OverlayLayer`].
//!
//! All writes go to the upper (agent) layer except for passthrough paths,
//! which write directly to the lower layer. COW copies happen outside the
//! whiteout lock so slow I/O never blocks readers.

use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use phantom_core::reserved::is_reserved_path;
use tracing::{debug, trace};

use crate::error::OverlayError;
use crate::types::{is_hidden, is_passthrough};
use crate::whiteout::{is_safe_relative_path, is_safe_symlink_target};

/// Reject upper-layer writes targeting Phantom's reserved paths
/// (`.phantom/`, `.whiteouts.json` at any depth, and `.git/` when it is NOT
/// being handled as passthrough).
///
/// `.git/` is passthrough by design (see [`PASSTHROUGH_DIRS`] in
/// [`crate::types`]) so git-aware agents can read and write the real
/// repository state from within the overlay mount. That contract is not
/// broken here — callers already short-circuit to the lower layer for
/// passthrough paths. This guard runs AFTER the passthrough check to catch
/// anything that would otherwise land in the upper layer.
///
/// `.phantom/` is already hidden via [`is_hidden`], so it is caught by the
/// existing `is_hidden` checks; this guard is an additional defense so a
/// future relaxation of hidden-dir handling cannot silently re-expose it.
///
/// Returns a distinct [`OverlayError::ReservedPath`] so callers and tests
/// can distinguish it from a simple not-found.
///
/// [`PASSTHROUGH_DIRS`]: crate::types
fn guard_reserved(rel_path: &Path) -> Result<(), OverlayError> {
    if let Some(kind) = is_reserved_path(rel_path) {
        tracing::warn!(path = %rel_path.display(), ?kind, "refusing overlay upper-layer write to reserved path");
        return Err(OverlayError::ReservedPath(rel_path.to_path_buf()));
    }
    Ok(())
}

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
        // After passthrough short-circuit: any remaining reserved-path write
        // would land in the upper layer and then be walked into a changeset.
        // Block it. (`.git/…` reached here only if passthrough routing changes
        // in the future; `.whiteouts.json` and `.phantom/…` are blocked here.)
        guard_reserved(rel_path)?;

        let upper_path = self.upper.join(rel_path);
        ensure_parent_dir(&upper_path)?;
        // I/O happens before acquiring the whiteout lock.
        fs::write(&upper_path, data)?;

        // If the file was previously whitelisted as deleted, persist the
        // updated whiteout set HARD — a stale on-disk `.whiteouts.json`
        // would hide this re-created file on the next process restart,
        // causing silent data loss for the agent.
        let removed = self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path);
        if removed {
            self.persist_whiteouts()?;
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
        guard_reserved(rel_path)?;

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

        // Hard-persist: a stale whiteout after a successful truncate-to-
        // re-create would hide the file from `modified_files()` and from
        // the agent's own view on restart.
        let removed = self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path);
        if removed {
            self.persist_whiteouts()?;
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
        guard_reserved(rel_path)?;

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
        // Reject links placed in `.phantom/`/`.whiteouts.json` (upper layer)
        // and also reject any link whose TARGET resolves into `.git/` or
        // `.phantom/` — a dangling link today is a hostile write tomorrow.
        guard_reserved(rel_path)?;
        guard_reserved(target)?;

        let upper_path = self.upper.join(rel_path);
        ensure_parent_dir(&upper_path)?;
        std::os::unix::fs::symlink(target, &upper_path)?;

        let removed = self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path);
        if removed {
            self.persist_whiteouts()?;
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
        // Non-passthrough deletes create whiteouts in the upper layer. A
        // whiteout for `.whiteouts.json` or `.phantom/foo` would corrupt
        // Phantom's own state; reject it. `.git/` is passthrough so this
        // branch is never reached for it.
        guard_reserved(rel_path)?;

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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        // Non-passthrough: any `ensure_upper_copy` would land the path in the
        // upper layer. Block reserved paths here as a last line of defense.
        guard_reserved(rel_path)?;

        let upper_path = self.upper.join(rel_path);

        if upper_path.exists() {
            // Already in the upper layer. Hard-persist the whiteout removal:
            // a stale on-disk whiteout would later hide this existing file
            // from `modified_files()` and from the agent's view.
            let removed = self
                .whiteouts
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(rel_path);
            if removed {
                self.persist_whiteouts()?;
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
        let removed = self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(rel_path);
        if removed {
            self.persist_whiteouts()?;
        }

        Ok(upper_path)
    }
}
