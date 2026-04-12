//! Copy-on-write overlay layer.
//!
//! [`OverlayLayer`] handles all COW logic using plain filesystem operations.
//! Reads check the upper (agent write) layer first, then fall through to the
//! lower (trunk) layer. Writes always go to the upper layer. Deletes are
//! tracked via a whiteout set persisted to `upper/.whiteouts.json`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::error::OverlayError;
use crate::trunk_view::{read_dir_entries, walk_files};

const WHITEOUT_FILE: &str = ".whiteouts.json";

/// Internal files placed in the upper layer by Phantom that should never appear
/// as agent modifications or be committed to git.
const INTERNAL_FILES: &[&str] = &[".whiteouts.json", ".phantom-task.md"];

/// Paths that are always hidden from the overlay view.
///
/// `.phantom` is the internal metadata directory (contains the FUSE mount point
/// itself — exposing it through FUSE causes a self-referential access deadlock).
const HIDDEN_DIRS: &[&str] = &[".phantom"];

/// Paths that bypass the COW upper layer entirely.
///
/// `.git` is passthrough so that git operations from within the overlay mount
/// (e.g. `git status`, `git commit`) read and write the real repository state.
/// All reads, writes, and metadata queries for these paths go directly to the
/// lower (trunk) layer.
const PASSTHROUGH_DIRS: &[&str] = &[".git"];

/// Returns `true` if a relative path starts with a hidden directory.
fn is_hidden(rel_path: &Path) -> bool {
    first_component(rel_path).is_some_and(|name| HIDDEN_DIRS.contains(&name))
}

/// Returns `true` if a relative path starts with a passthrough directory.
///
/// Passthrough paths are routed directly to the lower layer for all operations,
/// bypassing the upper layer and whiteout tracking.
fn is_passthrough(rel_path: &Path) -> bool {
    first_component(rel_path).is_some_and(|name| PASSTHROUGH_DIRS.contains(&name))
}

/// Extract the first path component as a `&str`, if possible.
fn first_component(rel_path: &Path) -> Option<&str> {
    rel_path
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
}

/// File type for directory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
}

/// A directory entry returned by [`OverlayLayer::read_dir`].
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Entry name (file or directory name, not full path).
    pub name: OsString,
    /// The kind of filesystem entry.
    pub file_type: FileType,
}

/// Serializable whiteout set for persistence.
#[derive(Debug, Default, Serialize, Deserialize)]
struct WhiteoutSet {
    paths: Vec<String>,
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

    /// Read a file's contents. Checks upper layer first, then lower.
    ///
    /// Passthrough paths (`.git/**`) are read directly from the lower layer.
    /// Returns an error if the path has been whiteout'd (deleted) or is hidden.
    pub fn read_file(&self, rel_path: &Path) -> Result<Vec<u8>, OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            if lower_path.exists() {
                trace!(path = %rel_path.display(), layer = "lower-passthrough", "reading file");
                return Ok(fs::read(&lower_path)?);
            }
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if self.whiteouts.contains(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let upper_path = self.upper.join(rel_path);
        if upper_path.exists() {
            trace!(path = %rel_path.display(), layer = "upper", "reading file");
            return Ok(fs::read(&upper_path)?);
        }

        let lower_path = self.lower.join(rel_path);
        if lower_path.exists() {
            trace!(path = %rel_path.display(), layer = "lower", "reading file");
            return Ok(fs::read(&lower_path)?);
        }

        Err(OverlayError::PathNotFound(rel_path.to_path_buf()))
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
            let _ = self.persist_whiteouts();
        }

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
        if is_hidden(rel_path) {
            return false;
        }
        if is_passthrough(rel_path) {
            return self.lower.join(rel_path).exists();
        }
        if self.whiteouts.contains(rel_path) {
            return false;
        }
        self.upper.join(rel_path).exists() || self.lower.join(rel_path).exists()
    }

    /// Get filesystem metadata for a path.
    ///
    /// Passthrough paths read metadata directly from the lower layer.
    /// Normal paths check upper first, then lower.
    pub fn getattr(&self, rel_path: &Path) -> Result<fs::Metadata, OverlayError> {
        if is_hidden(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if is_passthrough(rel_path) {
            let lower_path = self.lower.join(rel_path);
            if lower_path.exists() {
                return Ok(fs::metadata(&lower_path)?);
            }
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        if self.whiteouts.contains(rel_path) {
            return Err(OverlayError::PathNotFound(rel_path.to_path_buf()));
        }

        let upper_path = self.upper.join(rel_path);
        if upper_path.exists() {
            return Ok(fs::metadata(&upper_path)?);
        }

        let lower_path = self.lower.join(rel_path);
        if lower_path.exists() {
            return Ok(fs::metadata(&lower_path)?);
        }

        Err(OverlayError::PathNotFound(rel_path.to_path_buf()))
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

    /// Persist the whiteout set to `upper/.whiteouts.json`.
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
        fs::write(self.upper.join(WHITEOUT_FILE), json)?;
        Ok(())
    }

    /// Remove a path from the whiteout set (used when re-writing a deleted file).
    pub fn remove_whiteout(&mut self, rel_path: &Path) {
        if self.whiteouts.remove(rel_path) {
            // Best-effort persist; errors are non-fatal here.
            let _ = self.persist_whiteouts();
        }
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
            if path.is_dir() {
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

/// Load whiteouts from the persisted JSON file in the upper directory.
fn load_whiteouts(upper: &Path) -> Result<HashSet<PathBuf>, OverlayError> {
    let path = upper.join(WHITEOUT_FILE);
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let data = fs::read_to_string(&path)?;
    let ws: WhiteoutSet =
        serde_json::from_str(&data).map_err(|e| OverlayError::Serialization(e.to_string()))?;
    Ok(ws.paths.into_iter().map(PathBuf::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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
        let layer2 =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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

        let layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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

        let layer =
            OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

        // Reads must come from lower, not upper.
        let config = layer.read_file(Path::new(".git/config")).unwrap();
        assert_eq!(config, b"[core]\nbare = false");
    }
}
