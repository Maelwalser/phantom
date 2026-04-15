//! Copy-on-write overlay layer.
//!
//! [`OverlayLayer`] handles all COW logic using plain filesystem operations.
//! Reads check the upper (agent write) layer first, then fall through to the
//! lower (trunk) layer. Writes always go to the upper layer. Deletes are
//! tracked via a whiteout set persisted to `upper/.whiteouts.json`.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tracing::{debug, trace, warn};

use crate::error::OverlayError;
use crate::trunk_view::{read_dir_entries, walk_files};
use crate::types::{DirEntry, FileType, is_hidden, is_passthrough, reparent_children};
use crate::whiteout::{INTERNAL_FILES, WHITEOUT_FILE, WhiteoutSet, load_whiteouts};

/// Atomically write `data` to `target` via a temporary sibling file.
///
/// On Unix, `rename` within the same filesystem is atomic, so readers never
/// see a partially-written file.
fn atomic_write(target: &Path, data: &[u8]) -> Result<(), OverlayError> {
    let tmp = target.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, target)?;
    Ok(())
}

/// Copy-on-write overlay layer.
///
/// The lower layer is the trunk working tree (read-only source of truth).
/// The upper layer is the per-agent write directory. Deleted files from the
/// lower layer are tracked in a whiteout set so reads correctly report them
/// as absent.
pub struct OverlayLayer {
    lower: PathBuf,
    upper: PathBuf,
    whiteouts: HashSet<PathBuf>,
}

/// Result of classifying a relative path through the overlay layers.
///
/// Encodes the hidden → passthrough → whiteout → upper → lower decision tree
/// in a single place so callers don't repeat it.
enum ResolvedPath {
    /// File exists in the upper (agent write) layer.
    Upper(PathBuf),
    /// File exists only in the lower (trunk) layer.
    Lower(PathBuf),
    /// Passthrough path — routed directly to the lower layer, bypassing COW.
    Passthrough(PathBuf),
    /// Path is hidden, whited-out, or absent from both layers.
    NotFound,
}

impl OverlayLayer {
    /// Create a new overlay layer.
    ///
    /// Creates the upper directory if it does not already exist. Loads any
    /// previously persisted whiteouts from `upper/.whiteouts.json`.
    pub fn new(lower: PathBuf, upper: PathBuf) -> Result<Self, OverlayError> {
        fs::create_dir_all(&upper)?;

        let whiteouts = load_whiteouts(&upper)?;

        debug!(
            lower = %lower.display(),
            upper = %upper.display(),
            whiteout_count = whiteouts.len(),
            "overlay layer created"
        );

        Ok(Self {
            lower,
            upper,
            whiteouts,
        })
    }

    /// Classify a relative path through the overlay layers.
    ///
    /// Encodes the shared decision tree: hidden paths → `NotFound`,
    /// passthrough paths → `Passthrough`, whiteout'd paths → `NotFound`,
    /// then upper-before-lower fallback.
    fn classify(&self, rel_path: &Path) -> ResolvedPath {
        if is_hidden(rel_path) {
            return ResolvedPath::NotFound;
        }

        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            return if lower_path.exists() {
                ResolvedPath::Passthrough(lower_path)
            } else {
                ResolvedPath::NotFound
            };
        }

        if self.whiteouts.contains(rel_path) {
            return ResolvedPath::NotFound;
        }

        let upper_path = self.upper.join(rel_path);
        if upper_path.exists() {
            return ResolvedPath::Upper(upper_path);
        }

        let lower_path = self.lower.join(rel_path);
        if lower_path.exists() {
            return ResolvedPath::Lower(lower_path);
        }

        ResolvedPath::NotFound
    }

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

    /// Write a file to the upper layer (or directly to lower for passthrough paths).
    ///
    /// Creates parent directories as needed. Automatically removes the path
    /// from the whiteout set if it was previously deleted.
    pub fn write_file(&mut self, rel_path: &Path, data: &[u8]) -> Result<(), OverlayError> {
        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            if let Some(parent) = lower_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&lower_path, data)?;
            trace!(path = %rel_path.display(), layer = "lower-passthrough", "writing file");
            return Ok(());
        }

        let upper_path = self.upper.join(rel_path);
        if let Some(parent) = upper_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&upper_path, data)?;

        if self.whiteouts.remove(rel_path) {
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
    pub fn truncate_file(&mut self, rel_path: &Path, new_size: u64) -> Result<(), OverlayError> {
        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            let file = OpenOptions::new().write(true).open(&lower_path)?;
            file.set_len(new_size)?;
            trace!(path = %rel_path.display(), new_size, layer = "lower-passthrough", "truncate");
            return Ok(());
        }

        let upper_path = self.upper.join(rel_path);

        if !upper_path.exists() {
            // File only exists in the lower layer — COW copy to upper before truncating.
            let lower_path = self.lower.join(rel_path);
            if !lower_path.exists() && !self.whiteouts.contains(rel_path) {
                return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
            }

            if let Some(parent) = upper_path.parent() {
                fs::create_dir_all(parent)?;
            }

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
        } else {
            // File already in upper layer — truncate in place.
            let file = OpenOptions::new().write(true).open(&upper_path)?;
            file.set_len(new_size)?;
        }

        if self.whiteouts.remove(rel_path) {
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
    pub fn set_permissions(&mut self, rel_path: &Path, mode: u32) -> Result<(), OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let perms = fs::Permissions::from_mode(mode);

        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            if !lower_path.exists() {
                return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
            }
            fs::set_permissions(&lower_path, perms)?;
            trace!(path = %rel_path.display(), mode = format!("{mode:#o}"), layer = "lower-passthrough", "chmod");
            return Ok(());
        }

        if self.whiteouts.contains(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let upper_path = self.upper.join(rel_path);

        if !upper_path.exists() {
            // COW: copy from lower to upper before changing permissions.
            let lower_path = self.lower.join(rel_path);
            if !lower_path.exists() {
                return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
            }

            if let Some(parent) = upper_path.parent() {
                fs::create_dir_all(parent)?;
            }

            fs::copy(&lower_path, &upper_path)?;
        }

        fs::set_permissions(&upper_path, perms)?;
        trace!(path = %rel_path.display(), mode = format!("{mode:#o}"), layer = "upper", "chmod");
        Ok(())
    }

    /// Delete a file from the overlay.
    ///
    /// Passthrough paths are deleted directly from the lower layer (no whiteout).
    /// For normal paths, if the file exists in the upper layer it is removed, and
    /// the path is added to the whiteout set so the lower layer's version is hidden.
    pub fn delete_file(&mut self, rel_path: &Path) -> Result<(), OverlayError> {
        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            if lower_path.exists() {
                fs::remove_file(&lower_path)?;
            }
            debug!(path = %rel_path.display(), "passthrough file deleted");
            return Ok(());
        }

        let upper_path = self.upper.join(rel_path);
        if upper_path.exists() {
            fs::remove_file(&upper_path)?;
        }

        self.whiteouts.insert(rel_path.to_path_buf());
        self.persist_whiteouts()?;

        debug!(path = %rel_path.display(), "file deleted (whiteout created)");
        Ok(())
    }

    /// Read a directory, merging entries from upper and lower layers.
    ///
    /// Passthrough directories read only from the lower layer. For normal
    /// directories, entries from both layers are merged (upper takes precedence),
    /// excluding whiteout'd and hidden entries. Passthrough entries (e.g. `.git`)
    /// are included when listing the root directory.
    pub fn read_dir(&self, rel_path: &Path) -> Result<Vec<DirEntry>, OverlayError> {
        // Passthrough directories read exclusively from lower.
        if is_passthrough(rel_path) {
            let lower_dir = self.lower.join(rel_path);
            if lower_dir.is_dir() {
                return read_dir_entries(&lower_dir);
            }
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let mut seen = HashSet::new();
        let mut entries = Vec::new();

        // Upper layer first (takes precedence).
        let upper_dir = self.upper.join(rel_path);
        if upper_dir.is_dir() {
            for entry in read_dir_entries(&upper_dir)? {
                let entry_rel = rel_path.join(&entry.name);
                if !self.whiteouts.contains(&entry_rel) && !is_hidden(&entry_rel) {
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
        let lower_dir = self.lower.join(rel_path);
        if lower_dir.is_dir() {
            for entry in read_dir_entries(&lower_dir)? {
                let entry_rel = rel_path.join(&entry.name);
                if !seen.contains(&entry.name)
                    && !self.whiteouts.contains(&entry_rel)
                    && !is_hidden(&entry_rel)
                {
                    seen.insert(entry.name.clone());
                    entries.push(entry);
                }
            }
        }

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
            | ResolvedPath::Passthrough(path) => Ok(fs::metadata(&path)?),
            ResolvedPath::NotFound => Err(OverlayError::PathNotFound(rel_path.to_path_buf())),
        }
    }

    /// Return all files that have been written to the upper layer.
    ///
    /// Paths are relative to the overlay root. Phantom internal files
    /// (`.whiteouts.json`, `.phantom-task.md`) are excluded.
    pub fn modified_files(&self) -> Result<Vec<PathBuf>, OverlayError> {
        let all = walk_files(&self.upper, &self.upper)?;
        Ok(all
            .into_iter()
            .filter(|p| {
                let name = p.to_string_lossy();
                !INTERNAL_FILES.iter().any(|f| name == *f)
            })
            .collect())
    }

    /// Return the set of paths that have been deleted (whiteout'd).
    #[must_use]
    pub fn deleted_files(&self) -> Vec<PathBuf> {
        self.whiteouts.iter().cloned().collect()
    }

    /// Update the lower layer pointer (e.g. when trunk advances).
    pub fn update_lower(&mut self, new_lower: PathBuf) {
        debug!(
            old = %self.lower.display(),
            new = %new_lower.display(),
            "lower layer updated"
        );
        self.lower = new_lower;
    }

    /// Persist the whiteout set, logging a warning on failure.
    ///
    /// Used in code paths where the primary operation has already succeeded
    /// and whiteout persistence is a best-effort follow-up.
    fn persist_whiteouts_or_warn(&self) {
        if let Err(e) = self.persist_whiteouts() {
            warn!(error = %e, "failed to persist whiteouts (best-effort)");
        }
    }

    /// Persist the whiteout set to `upper/.whiteouts.json`.
    ///
    /// Uses write-then-rename so a crash mid-write cannot leave a truncated
    /// file that fails to deserialize on the next [`load_whiteouts`] call.
    pub fn persist_whiteouts(&self) -> Result<(), OverlayError> {
        let ws = WhiteoutSet {
            paths: self
                .whiteouts
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        };
        let json = serde_json::to_string_pretty(&ws)
            .map_err(|e| OverlayError::Serialization(e.to_string()))?;
        atomic_write(&self.upper.join(WHITEOUT_FILE), json.as_bytes())?;
        Ok(())
    }

    /// Remove a path from the whiteout set (used when re-writing a deleted file).
    pub fn remove_whiteout(&mut self, rel_path: &Path) {
        if self.whiteouts.remove(rel_path) {
            // Best-effort persist; errors are non-fatal here.
            self.persist_whiteouts_or_warn();
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

    /// Ensure a file exists in the upper layer, performing a streaming COW copy
    /// from the lower layer if necessary.
    ///
    /// Returns the absolute path to the file in the upper layer. For passthrough
    /// paths, returns the lower-layer path directly (writes go there).
    ///
    /// Used by the FUSE layer on writable `open()` so that subsequent
    /// `pwrite(2)` calls can operate directly on the upper-layer file.
    pub fn ensure_upper_copy(&mut self, rel_path: &Path) -> Result<PathBuf, OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            if lower_path.exists() {
                return Ok(lower_path);
            }
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let upper_path = self.upper.join(rel_path);

        if upper_path.exists() {
            // Already in the upper layer.
            if self.whiteouts.remove(rel_path) {
                self.persist_whiteouts_or_warn();
            }
            return Ok(upper_path);
        }

        // Not in upper — check lower for COW copy.
        let lower_path = self.lower.join(rel_path);
        if !lower_path.exists() && !self.whiteouts.contains(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if let Some(parent) = upper_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if lower_path.exists() {
            // Streaming COW copy — never loads entire file into memory.
            let mut src = fs::File::open(&lower_path)?;
            let mut dst = fs::File::create(&upper_path)?;
            std::io::copy(&mut src, &mut dst)?;
            trace!(path = %rel_path.display(), "COW copy to upper layer");
        } else {
            // Whiteout'd file being re-opened for write — create empty.
            fs::File::create(&upper_path)?;
        }

        if self.whiteouts.remove(rel_path) {
            self.persist_whiteouts_or_warn();
        }

        Ok(upper_path)
    }

    /// Return a reference to the upper directory path.
    #[must_use]
    pub fn upper_dir(&self) -> &Path {
        &self.upper
    }

    /// Return a reference to the lower directory path.
    #[must_use]
    pub fn lower_dir(&self) -> &Path {
        &self.lower
    }

    /// Returns `true` if the given relative path is a passthrough path.
    ///
    /// Passthrough paths bypass the COW upper layer and route directly to the
    /// lower (trunk) layer for all operations.
    #[must_use]
    pub fn is_passthrough(&self, rel_path: &Path) -> bool {
        is_passthrough(rel_path)
    }

    /// Rename a file or directory within the overlay.
    ///
    /// Handles all COW semantics: if the source is only in the lower layer,
    /// it is copied up to the upper layer at the new path and the old path
    /// is whiteout'd. If the source is in the upper layer, it is moved
    /// directly. Passthrough paths (`.git/`) are renamed in the lower layer.
    ///
    /// Per POSIX, if the destination already exists it is replaced (for files)
    /// or the rename fails with `ENOTEMPTY` (for non-empty directories).
    pub fn rename_file(&mut self, old: &Path, new: &Path) -> Result<(), OverlayError> {
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

        // Source must exist in the overlay.
        if !self.exists(old) {
            return Err(OverlayError::PathNotFound(old.to_path_buf()));
        }

        // Snapshot child whiteouts BEFORE modifying the set — needed for migration.
        let child_whiteouts = self.collect_child_whiteouts(old);

        // Ensure destination parent directories exist in upper.
        let upper_new = self.upper.join(new);
        if let Some(parent) = upper_new.parent() {
            fs::create_dir_all(parent)?;
        }

        // Move or copy-up the source.
        let mut whiteouts_changed = self.move_or_copy_up(old, new)?;

        // Reconcile whiteouts after the filesystem move.
        if self.reconcile_whiteouts_after_rename(old, new, child_whiteouts) {
            whiteouts_changed = true;
        }

        if whiteouts_changed {
            self.persist_whiteouts()?;
        }

        debug!(old = %old.display(), new = %new.display(), "rename");
        Ok(())
    }

    /// Rename directly in the lower layer (passthrough paths like `.git/`).
    fn rename_passthrough(&self, old: &Path, new: &Path) -> Result<(), OverlayError> {
        let lower_old = self.lower.join(old);
        let lower_new = self.lower.join(new);
        if let Some(parent) = lower_new.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&lower_old, &lower_new)?;
        debug!(old = %old.display(), new = %new.display(), "passthrough rename");
        Ok(())
    }

    /// Collect all strict child whiteouts under `prefix`.
    fn collect_child_whiteouts(&self, prefix: &Path) -> Vec<PathBuf> {
        // Reuse reparent_children with a dummy new_prefix to collect children.
        reparent_children(self.whiteouts.iter(), prefix, prefix)
            .into_iter()
            .map(|(old, _)| old)
            .collect()
    }

    /// Move the source to the destination, performing a COW copy-up if the
    /// source only exists in the lower layer.
    ///
    /// Returns `true` if whiteouts were modified.
    fn move_or_copy_up(&mut self, old: &Path, new: &Path) -> Result<bool, OverlayError> {
        let upper_old = self.upper.join(old);
        let upper_new = self.upper.join(new);
        let upper_old_exists = upper_old.exists();
        let lower_old_exists = self.lower.join(old).exists() && !self.whiteouts.contains(old);

        if upper_old_exists {
            // Source is in upper layer — move directly.
            fs::rename(&upper_old, &upper_new)?;

            // If source also exists in lower, whiteout the old path to hide
            // the lower-layer ghost.
            if lower_old_exists {
                self.whiteouts.insert(old.to_path_buf());
            }
            Ok(lower_old_exists)
        } else {
            // Source is only in lower layer — copy up to new position.
            let lower_old = self.lower.join(old);
            if lower_old.is_dir() {
                self.copy_up_dir(old, new)?;
                // Whiteout all files within the lower directory so they don't
                // bleed through (the whiteout model is per-file, not
                // hierarchical).
                let lower_children = walk_files(&lower_old, &self.lower)?;
                for child in lower_children {
                    self.whiteouts.insert(child);
                }
            } else {
                let content = fs::read(&lower_old)?;
                fs::write(&upper_new, &content)?;
            }

            self.whiteouts.insert(old.to_path_buf());
            Ok(true)
        }
    }

    /// Clean up destination whiteouts and migrate child whiteouts from old
    /// prefix to new prefix after a rename.
    ///
    /// Returns `true` if any whiteouts were modified.
    fn reconcile_whiteouts_after_rename(
        &mut self,
        old: &Path,
        new: &Path,
        pre_existing_child_whiteouts: Vec<PathBuf>,
    ) -> bool {
        let mut changed = false;

        // The new path is now live — remove its whiteout.
        if self.whiteouts.remove(new) {
            changed = true;
        }

        // Remove child whiteouts under the new path (strict children only).
        let to_remove = self.collect_child_whiteouts(new);
        for w in &to_remove {
            self.whiteouts.remove(w);
            changed = true;
        }

        // Migrate pre-existing child whiteouts from old prefix to new prefix.
        let migrations = reparent_children(pre_existing_child_whiteouts.iter(), old, new);
        for (old_w, new_w) in migrations {
            self.whiteouts.remove(&old_w);
            self.whiteouts.insert(new_w);
            changed = true;
        }

        changed
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
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(&dst, &content)?;
                }
                FileType::Symlink => {
                    let src = self.resolve_path(&child_old)?;
                    let link_target = fs::read_link(&src)?;
                    let dst = self.upper.join(&child_new);
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    std::os::unix::fs::symlink(&link_target, &dst)?;
                }
            }
        }

        Ok(())
    }

    /// Clear all files from the upper layer and reset whiteouts.
    ///
    /// After a successful materialization, the agent's changes live in trunk.
    /// Clearing the upper layer ensures subsequent reads fall through to the
    /// now-updated trunk instead of returning stale overlay copies.
    pub fn clear_upper(&mut self) -> Result<(), OverlayError> {
        // Remove every entry in the upper directory except the directory itself.
        for entry in fs::read_dir(&self.upper)? {
            let entry = entry?;
            let path = entry.path();
            // Use entry.file_type() instead of path.is_dir() to avoid following
            // symlinks. path.is_dir() traverses symlinks, so a symlink pointing
            // to an external directory would cause remove_dir_all to delete the
            // target directory's contents.
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }

        self.whiteouts.clear();

        debug!(upper = %self.upper.display(), "upper layer cleared after materialization");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    /// Helper: create lower and upper temp dirs, return (lower, upper, _guards).
    fn setup() -> (TempDir, TempDir) {
        let lower = TempDir::new().unwrap();
        let upper = TempDir::new().unwrap();
        (lower, upper)
    }

    #[test]
    fn write_to_upper_and_read_back() {
        let (lower, upper) = setup();
        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // Write and remove from whiteouts in case.
        layer.write_file(Path::new("hello.txt"), b"world").unwrap();
        let data = layer.read_file(Path::new("hello.txt")).unwrap();
        assert_eq!(data, b"world");
    }

    #[test]
    fn read_falls_through_to_lower() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("trunk.txt"), b"from trunk").unwrap();

        let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        let data = layer.read_file(Path::new("trunk.txt")).unwrap();
        assert_eq!(data, b"from trunk");
    }

    #[test]
    fn upper_wins_over_lower() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("shared.txt"), b"lower").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        layer.write_file(Path::new("shared.txt"), b"upper").unwrap();

        let data = layer.read_file(Path::new("shared.txt")).unwrap();
        assert_eq!(data, b"upper");
    }

    #[test]
    fn delete_hides_lower_file() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("victim.txt"), b"doomed").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        layer.delete_file(Path::new("victim.txt")).unwrap();

        assert!(!layer.exists(Path::new("victim.txt")));
        assert!(layer.read_file(Path::new("victim.txt")).is_err());

        // Verify excluded from read_dir as well.
        let entries = layer.read_dir(Path::new("")).unwrap();
        let names: Vec<_> = entries
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert!(!names.contains(&"victim.txt".to_string()));
    }

    #[test]
    fn delete_then_rewrite_restores_file() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("file.txt"), b"v1").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        layer.delete_file(Path::new("file.txt")).unwrap();
        assert!(!layer.exists(Path::new("file.txt")));

        layer.write_file(Path::new("file.txt"), b"v2").unwrap();

        assert!(layer.exists(Path::new("file.txt")));
        let data = layer.read_file(Path::new("file.txt")).unwrap();
        assert_eq!(data, b"v2");
    }

    #[test]
    fn modified_files_returns_upper_only() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("lower.txt"), b"trunk").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        layer.write_file(Path::new("new.txt"), b"agent").unwrap();

        let modified = layer.modified_files().unwrap();
        assert!(modified.contains(&PathBuf::from("new.txt")));
        assert!(!modified.contains(&PathBuf::from("lower.txt")));
        // The whiteouts file should not appear.
        assert!(
            !modified
                .iter()
                .any(|p| p.to_string_lossy() == ".whiteouts.json")
        );
    }

    #[test]
    fn update_lower_changes_fallthrough() {
        let (lower1, upper) = setup();
        let lower2 = TempDir::new().unwrap();

        fs::write(lower1.path().join("a.txt"), b"lower1").unwrap();
        fs::write(lower2.path().join("b.txt"), b"lower2").unwrap();

        let mut layer =
            OverlayLayer::new(lower1.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        assert!(layer.exists(Path::new("a.txt")));
        assert!(!layer.exists(Path::new("b.txt")));

        layer.update_lower(lower2.path().to_path_buf());
        assert!(!layer.exists(Path::new("a.txt")));
        assert!(layer.exists(Path::new("b.txt")));
    }

    #[test]
    fn directory_merging() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("from_lower.txt"), b"l").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        layer.write_file(Path::new("from_upper.txt"), b"u").unwrap();

        let entries = layer.read_dir(Path::new("")).unwrap();
        let names: HashSet<_> = entries
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains("from_lower.txt"));
        assert!(names.contains("from_upper.txt"));
        // No duplicates.
        assert_eq!(entries.len(), names.len());
    }

    #[test]
    fn nested_directory_creation() {
        let (lower, upper) = setup();
        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        layer.write_file(Path::new("a/b/c.txt"), b"deep").unwrap();

        let data = layer.read_file(Path::new("a/b/c.txt")).unwrap();
        assert_eq!(data, b"deep");
    }

    #[test]
    fn whiteout_persistence_across_instances() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("persist.txt"), b"data").unwrap();

        {
            let mut layer =
                OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
            layer.delete_file(Path::new("persist.txt")).unwrap();
        }

        // New instance from the same upper dir should restore whiteouts.
        let layer2 = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        assert!(!layer2.exists(Path::new("persist.txt")));
        assert!(layer2.read_file(Path::new("persist.txt")).is_err());
    }

    #[test]
    fn clear_upper_removes_files_and_whiteouts() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("trunk.txt"), b"from trunk").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // Write some files and create a whiteout.
        layer
            .write_file(Path::new("agent.txt"), b"agent work")
            .unwrap();
        layer
            .write_file(Path::new("sub/nested.txt"), b"deep")
            .unwrap();
        layer.delete_file(Path::new("trunk.txt")).unwrap();

        assert!(layer.exists(Path::new("agent.txt")));
        assert!(!layer.exists(Path::new("trunk.txt")));

        // Clear the upper layer.
        layer.clear_upper().unwrap();

        // Agent files gone — upper is empty.
        assert!(layer.modified_files().unwrap().is_empty());
        // Trunk file visible again (whiteout cleared).
        assert!(layer.exists(Path::new("trunk.txt")));
        let data = layer.read_file(Path::new("trunk.txt")).unwrap();
        assert_eq!(data, b"from trunk");
        // Agent-only file is gone.
        assert!(!layer.exists(Path::new("agent.txt")));
    }

    #[test]
    fn hidden_dirs_are_invisible() {
        let (lower, upper) = setup();

        // Create .phantom directory in the lower layer.
        fs::create_dir_all(lower.path().join(".phantom/overlays/agent/mount")).unwrap();
        fs::write(lower.path().join("visible.txt"), b"hello").unwrap();

        let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // Hidden paths must be invisible.
        assert!(!layer.exists(Path::new(".phantom")));
        assert!(!layer.exists(Path::new(".phantom/overlays/agent/mount")));
        assert!(layer.getattr(Path::new(".phantom")).is_err());

        // Visible files still work.
        assert!(layer.exists(Path::new("visible.txt")));

        // read_dir must not include hidden entries.
        let entries = layer.read_dir(Path::new("")).unwrap();
        let names: Vec<_> = entries
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert!(!names.contains(&".phantom".to_string()));
        assert!(names.contains(&"visible.txt".to_string()));
    }

    #[test]
    fn git_dir_is_passthrough_to_lower() {
        let (lower, upper) = setup();

        // Create .git in the lower layer (simulates a real repo).
        fs::create_dir_all(lower.path().join(".git/objects")).unwrap();
        fs::create_dir_all(lower.path().join(".git/refs/heads")).unwrap();
        fs::write(lower.path().join(".git/HEAD"), b"ref: refs/heads/main").unwrap();
        fs::write(lower.path().join("visible.txt"), b"hello").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // .git should be visible and accessible.
        assert!(layer.exists(Path::new(".git")));
        assert!(layer.exists(Path::new(".git/HEAD")));
        assert!(layer.getattr(Path::new(".git")).is_ok());
        assert!(layer.getattr(Path::new(".git/HEAD")).is_ok());

        // Reads go to lower layer.
        let head = layer.read_file(Path::new(".git/HEAD")).unwrap();
        assert_eq!(head, b"ref: refs/heads/main");

        // Writes go directly to lower layer.
        layer
            .write_file(Path::new(".git/HEAD"), b"ref: refs/heads/feature")
            .unwrap();
        let updated = fs::read(lower.path().join(".git/HEAD")).unwrap();
        assert_eq!(updated, b"ref: refs/heads/feature");

        // Verify write did NOT go to upper layer.
        assert!(!upper.path().join(".git/HEAD").exists());

        // read_dir on .git returns lower layer contents.
        let entries = layer.read_dir(Path::new(".git")).unwrap();
        let names: HashSet<_> = entries
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains("HEAD"));
        assert!(names.contains("objects"));
        assert!(names.contains("refs"));

        // Root read_dir includes .git.
        let root_entries = layer.read_dir(Path::new("")).unwrap();
        let root_names: Vec<_> = root_entries
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert!(root_names.contains(&".git".to_string()));
        assert!(root_names.contains(&"visible.txt".to_string()));
    }

    #[test]
    fn git_passthrough_delete_hits_lower() {
        let (lower, upper) = setup();

        fs::create_dir_all(lower.path().join(".git")).unwrap();
        fs::write(lower.path().join(".git/MERGE_HEAD"), b"abc123").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // Delete a .git file — should remove from lower directly.
        layer.delete_file(Path::new(".git/MERGE_HEAD")).unwrap();
        assert!(!lower.path().join(".git/MERGE_HEAD").exists());
        // No whiteout should be created for passthrough paths.
        assert!(layer.deleted_files().is_empty());
    }

    #[test]
    fn git_passthrough_not_affected_by_upper_or_whiteouts() {
        let (lower, upper) = setup();

        fs::create_dir_all(lower.path().join(".git")).unwrap();
        fs::write(lower.path().join(".git/config"), b"[core]\nbare = false").unwrap();

        // Place a decoy in the upper layer — passthrough should ignore it.
        fs::create_dir_all(upper.path().join(".git")).unwrap();
        fs::write(upper.path().join(".git/config"), b"DECOY").unwrap();

        let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // Reads must come from lower, not upper.
        let config = layer.read_file(Path::new(".git/config")).unwrap();
        assert_eq!(config, b"[core]\nbare = false");
    }

    // ── rename_file tests ──────────────────────────────────────────────

    #[test]
    fn rename_file_in_upper() {
        let (lower, upper) = setup();
        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        layer.write_file(Path::new("old.txt"), b"data").unwrap();
        layer
            .rename_file(Path::new("old.txt"), Path::new("new.txt"))
            .unwrap();

        assert!(!layer.exists(Path::new("old.txt")));
        assert_eq!(layer.read_file(Path::new("new.txt")).unwrap(), b"data");
    }

    #[test]
    fn rename_file_from_lower_copies_up() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("src.txt"), b"from trunk").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        layer
            .rename_file(Path::new("src.txt"), Path::new("dst.txt"))
            .unwrap();

        // New path is readable, old path is gone.
        assert_eq!(
            layer.read_file(Path::new("dst.txt")).unwrap(),
            b"from trunk"
        );
        assert!(!layer.exists(Path::new("src.txt")));

        // Whiteout was created for old path.
        assert!(layer.deleted_files().contains(&PathBuf::from("src.txt")));

        // New file lives in upper layer.
        assert!(upper.path().join("dst.txt").exists());
    }

    #[test]
    fn rename_overwrites_existing_destination() {
        let (lower, upper) = setup();
        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        layer.write_file(Path::new("a.txt"), b"content-a").unwrap();
        layer.write_file(Path::new("b.txt"), b"content-b").unwrap();

        layer
            .rename_file(Path::new("a.txt"), Path::new("b.txt"))
            .unwrap();

        assert!(!layer.exists(Path::new("a.txt")));
        assert_eq!(layer.read_file(Path::new("b.txt")).unwrap(), b"content-a");
    }

    #[test]
    fn rename_upper_file_whiteouts_lower_ghost() {
        let (lower, upper) = setup();
        fs::write(lower.path().join("shared.txt"), b"lower").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        // Write to upper so the file exists in both layers.
        layer.write_file(Path::new("shared.txt"), b"upper").unwrap();

        layer
            .rename_file(Path::new("shared.txt"), Path::new("moved.txt"))
            .unwrap();

        // Old path hidden (lower ghost whiteout'd).
        assert!(!layer.exists(Path::new("shared.txt")));
        assert!(layer.deleted_files().contains(&PathBuf::from("shared.txt")));

        // New path has upper content.
        assert_eq!(layer.read_file(Path::new("moved.txt")).unwrap(), b"upper");
    }

    #[test]
    fn rename_directory_in_upper() {
        let (lower, upper) = setup();
        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        layer
            .write_file(Path::new("dir/child.txt"), b"hello")
            .unwrap();
        layer
            .rename_file(Path::new("dir"), Path::new("renamed"))
            .unwrap();

        assert!(!layer.exists(Path::new("dir/child.txt")));
        assert_eq!(
            layer.read_file(Path::new("renamed/child.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn rename_directory_from_lower() {
        let (lower, upper) = setup();
        fs::create_dir_all(lower.path().join("pkg")).unwrap();
        fs::write(lower.path().join("pkg/mod.rs"), b"pub mod foo;").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
        layer
            .rename_file(Path::new("pkg"), Path::new("lib"))
            .unwrap();

        assert!(!layer.exists(Path::new("pkg/mod.rs")));
        assert_eq!(
            layer.read_file(Path::new("lib/mod.rs")).unwrap(),
            b"pub mod foo;"
        );
        assert!(layer.deleted_files().contains(&PathBuf::from("pkg")));
    }

    #[test]
    fn rename_within_passthrough() {
        let (lower, _upper) = setup();
        fs::create_dir_all(lower.path().join(".git/refs")).unwrap();
        fs::write(lower.path().join(".git/refs/old"), b"ref").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), _upper.path().to_path_buf()).unwrap();
        layer
            .rename_file(Path::new(".git/refs/old"), Path::new(".git/refs/new"))
            .unwrap();

        assert!(!lower.path().join(".git/refs/old").exists());
        assert_eq!(
            fs::read(lower.path().join(".git/refs/new")).unwrap(),
            b"ref"
        );
        // No whiteouts for passthrough operations.
        assert!(layer.deleted_files().is_empty());
    }

    #[test]
    fn rename_cross_passthrough_fails() {
        let (lower, upper) = setup();
        fs::create_dir_all(lower.path().join(".git")).unwrap();
        fs::write(lower.path().join(".git/x"), b"data").unwrap();
        fs::write(lower.path().join("y"), b"data").unwrap();

        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // Passthrough → normal should fail.
        assert!(
            layer
                .rename_file(Path::new(".git/x"), Path::new("z"))
                .is_err()
        );
        // Normal → passthrough should fail.
        assert!(
            layer
                .rename_file(Path::new("y"), Path::new(".git/w"))
                .is_err()
        );
    }

    #[test]
    fn rename_hidden_path_fails() {
        let (lower, upper) = setup();
        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        layer.write_file(Path::new("a.txt"), b"data").unwrap();
        assert!(
            layer
                .rename_file(Path::new("a.txt"), Path::new(".phantom/x"))
                .is_err()
        );
        assert!(
            layer
                .rename_file(Path::new(".phantom/x"), Path::new("b.txt"))
                .is_err()
        );
    }

    #[test]
    fn rename_nonexistent_source_fails() {
        let (lower, upper) = setup();
        let mut layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        let result = layer.rename_file(Path::new("ghost.txt"), Path::new("dst.txt"));
        assert!(result.is_err());
    }
}
