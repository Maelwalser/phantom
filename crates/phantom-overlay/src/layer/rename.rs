//! Rename coordination for [`OverlayLayer`].
//!
//! Rename is the most intricate overlay operation: it must preserve POSIX
//! semantics while keeping upper/lower/whiteout state consistent. The
//! implementation uses a three-phase approach to minimize lock hold time:
//!
//! 1. **Snapshot** — capture whiteout state under a read lock.
//! 2. **I/O** — perform filesystem operations (rename / copy-up) with no lock held.
//! 3. **Reconcile** — atomically update the whiteout set under a write lock.

use std::fs;
use std::path::{Path, PathBuf};

use phantom_core::reserved::is_reserved_path;
use tracing::debug;

use crate::error::OverlayError;
use crate::trunk_view::walk_files;
use crate::types::{FileType, is_hidden, is_passthrough, reparent_children};
use crate::whiteout::is_safe_symlink_target;

use super::OverlayLayer;
use super::io_util::ensure_parent_dir;

impl OverlayLayer {
    /// Rename a file or directory within the overlay.
    ///
    /// Handles all COW semantics: if the source is only in the lower layer,
    /// it is copied up to the upper layer at the new path and the old path
    /// is whiteout'd. If the source is in the upper layer, it is moved
    /// directly. Passthrough paths (`.git/`) are renamed in the lower layer.
    ///
    /// Per POSIX, if the destination already exists it is replaced (for files)
    /// or the rename fails with `ENOTEMPTY` (for non-empty directories).
    pub fn rename_file(&self, old: &Path, new: &Path) -> Result<(), OverlayError> {
        // Hidden paths are never accessible.
        if is_hidden(old) || is_hidden(new) {
            return Err(OverlayError::PathNotFound(old.to_path_buf()));
        }

        let old_passthrough = is_passthrough(old);
        let new_passthrough = is_passthrough(new);

        // Cross-layer rename between passthrough and COW is invalid.
        if old_passthrough != new_passthrough {
            return Err(OverlayError::Io(std::io::Error::from_raw_os_error(22))); // EINVAL
        }

        if old_passthrough {
            return self.rename_passthrough(old, new);
        }

        // Non-passthrough rename: both endpoints would touch the upper layer
        // (directly, or via whiteout/COW). A rename into `.phantom/` or
        // `.whiteouts.json` would corrupt Phantom's own state; reject both
        // directions.  `.git/` is passthrough so this branch is never reached
        // for it, but a rename *across* layers was already rejected above.
        if let Some(kind) = is_reserved_path(old) {
            tracing::warn!(path = %old.display(), ?kind, "refusing overlay rename: source is reserved");
            return Err(OverlayError::ReservedPath(old.to_path_buf()));
        }
        if let Some(kind) = is_reserved_path(new) {
            tracing::warn!(path = %new.display(), ?kind, "refusing overlay rename: destination is reserved");
            return Err(OverlayError::ReservedPath(new.to_path_buf()));
        }

        // Source must exist in the overlay.
        if !self.exists(old) {
            return Err(OverlayError::PathNotFound(old.to_path_buf()));
        }

        // Phase 1: Snapshot whiteout state under read lock.
        let (child_whiteouts, lower_old_exists, lower) = {
            let wo = self
                .whiteouts
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let lower = self.lower_path();
            let child_wo = reparent_children(wo.iter(), old, old)
                .into_iter()
                .map(|(old_w, _)| old_w)
                .collect::<Vec<_>>();
            let lower_old_exists = lower.join(old).exists() && !wo.contains(old);
            (child_wo, lower_old_exists, lower)
        };

        // Ensure destination parent directories exist in upper.
        let upper_new = self.upper.join(new);
        ensure_parent_dir(&upper_new)?;

        // Phase 2: Filesystem operations (no lock held).
        let whiteout_inserts = self.do_rename_io(old, new, lower_old_exists, &lower)?;

        // Phase 3: Reconcile whiteouts atomically under write lock.
        let mut wo = self
            .whiteouts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changed = false;

        for path in &whiteout_inserts {
            wo.insert(path.clone());
            changed = true;
        }

        // The new path is now live — remove its whiteout.
        if wo.remove(new) {
            changed = true;
        }

        // Remove child whiteouts under the new path (strict children only).
        let to_remove: Vec<PathBuf> = reparent_children(wo.iter(), new, new)
            .into_iter()
            .map(|(old_w, _)| old_w)
            .collect();
        for w in &to_remove {
            wo.remove(w);
            changed = true;
        }

        // Migrate pre-existing child whiteouts from old prefix to new prefix.
        let migrations = reparent_children(child_whiteouts.iter(), old, new);
        for (old_w, new_w) in migrations {
            wo.remove(&old_w);
            wo.insert(new_w);
            changed = true;
        }

        drop(wo);

        if changed {
            self.persist_whiteouts()?;
        }

        debug!(old = %old.display(), new = %new.display(), "rename");
        Ok(())
    }

    /// Rename directly in the lower layer (passthrough paths like `.git/`).
    fn rename_passthrough(&self, old: &Path, new: &Path) -> Result<(), OverlayError> {
        let lower = self.lower_path();
        let lower_old = lower.join(old);
        let lower_new = lower.join(new);
        ensure_parent_dir(&lower_new)?;
        fs::rename(&lower_old, &lower_new)?;
        debug!(old = %old.display(), new = %new.display(), "passthrough rename");
        Ok(())
    }

    /// Perform the filesystem operations for a rename (no whiteout lock held).
    ///
    /// Returns the set of paths that should be inserted into the whiteout set.
    fn do_rename_io(
        &self,
        old: &Path,
        new: &Path,
        lower_old_exists: bool,
        lower: &Path,
    ) -> Result<Vec<PathBuf>, OverlayError> {
        let upper_old = self.upper.join(old);
        let upper_new = self.upper.join(new);
        let upper_old_exists = upper_old.exists();

        let mut inserts = Vec::new();

        if upper_old_exists {
            // Source is in upper layer — move directly.
            fs::rename(&upper_old, &upper_new)?;

            // If source also exists in lower, whiteout the old path to hide
            // the lower-layer ghost.
            if lower_old_exists {
                inserts.push(old.to_path_buf());
            }
        } else {
            // Source is only in lower layer — copy up to new position.
            let lower_old = lower.join(old);
            if lower_old.is_dir() {
                self.copy_up_dir(old, new)?;
                // Whiteout all files within the lower directory so they don't
                // bleed through (the whiteout model is per-file, not
                // hierarchical).
                let lower_children = walk_files(&lower_old, lower)?;
                for child in lower_children {
                    inserts.push(child);
                }
            } else {
                let content = fs::read(&lower_old)?;
                fs::write(&upper_new, &content)?;
            }

            inserts.push(old.to_path_buf());
        }

        Ok(inserts)
    }

    /// Copy a directory from the overlay-merged view to the upper layer at a
    /// new path. Used when renaming a directory that exists only in the lower
    /// layer.
    fn copy_up_dir(&self, old_rel: &Path, new_rel: &Path) -> Result<(), OverlayError> {
        let upper_dst = self.upper.join(new_rel);
        fs::create_dir_all(&upper_dst)?;

        let entries = self.read_dir(old_rel)?;
        for entry in entries {
            let child_old = old_rel.join(&entry.name);
            let child_new = new_rel.join(&entry.name);
            match entry.file_type {
                FileType::Directory => {
                    self.copy_up_dir(&child_old, &child_new)?;
                }
                FileType::File => {
                    let content = self.read_file(&child_old)?;
                    let dst = self.upper.join(&child_new);
                    ensure_parent_dir(&dst)?;
                    fs::write(&dst, &content)?;
                }
                FileType::Symlink => {
                    let src = self.resolve_path(&child_old)?;
                    let link_target = fs::read_link(&src)?;
                    // Drop symlinks whose target would escape the overlay at
                    // the new location.  The link may have been safe at its
                    // old position but unsafe relative to the new depth.
                    if !is_safe_symlink_target(&link_target, &child_new) {
                        continue;
                    }
                    let dst = self.upper.join(&child_new);
                    ensure_parent_dir(&dst)?;
                    std::os::unix::fs::symlink(&link_target, &dst)?;
                }
            }
        }

        Ok(())
    }
}
