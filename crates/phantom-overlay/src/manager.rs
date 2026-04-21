//! Overlay lifecycle management.
//!
//! [`OverlayManager`] creates, destroys, and tracks per-agent overlays.
//! On Linux it optionally mounts a FUSE filesystem at each overlay's mount
//! point; on other platforms the layer is still usable via direct API calls.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use phantom_core::AgentId;
use tracing::{debug, info};

use crate::error::OverlayError;
use crate::layer::OverlayLayer;

/// Handle for a mounted (or logically active) overlay.
pub struct MountHandle {
    /// The agent this overlay belongs to.
    pub agent_id: AgentId,
    /// Directory where the FUSE filesystem is mounted (Linux) or a logical mount point.
    pub mount_point: PathBuf,
    /// Directory containing the agent's write layer.
    pub upper_dir: PathBuf,
    /// The COW layer backing this overlay.
    layer: OverlayLayer,
}

/// Manages the lifecycle of per-agent overlays.
pub struct OverlayManager {
    phantom_dir: PathBuf,
    active_overlays: HashMap<AgentId, MountHandle>,
}

impl OverlayManager {
    /// Create a new overlay manager rooted at the given `.phantom/` directory.
    #[must_use]
    pub fn new(phantom_dir: PathBuf) -> Self {
        Self {
            phantom_dir,
            active_overlays: HashMap::new(),
        }
    }

    /// Create a new overlay for an agent.
    ///
    /// Sets up directory structure at `.phantom/overlays/<agent_id>/upper/`
    /// and `.phantom/overlays/<agent_id>/mount/`. On Linux, a FUSE filesystem
    /// can be mounted at the mount point separately.
    pub fn create_overlay(
        &mut self,
        agent_id: AgentId,
        trunk_path: &Path,
    ) -> Result<&MountHandle, OverlayError> {
        if self.active_overlays.contains_key(&agent_id) {
            return Err(OverlayError::AlreadyExists(agent_id));
        }

        let overlay_root = self.phantom_dir.join("overlays").join(&agent_id.0);
        let upper_dir = overlay_root.join("upper");
        let mount_point = overlay_root.join("mount");

        fs::create_dir_all(&upper_dir)?;
        fs::create_dir_all(&mount_point)?;

        let layer = OverlayLayer::new(trunk_path.to_path_buf(), upper_dir.clone())?;

        let handle = MountHandle {
            agent_id: agent_id.clone(),
            mount_point,
            upper_dir,
            layer,
        };

        self.active_overlays.insert(agent_id.clone(), handle);
        Ok(self.active_overlays.get(&agent_id).unwrap())
    }

    /// Destroy an agent's overlay, removing its directories.
    ///
    /// # Safety critical — VCS corruption guard
    ///
    /// If the overlay's `mount/` is still a live FUSE mount point, a naive
    /// `fs::remove_dir_all(overlay_root)` would recurse INTO the mount and
    /// call `unlink(2)` on every file it walks. Those calls are routed to
    /// [`OverlayLayer::delete_file`], which for passthrough paths
    /// (`.git/`, see [`crate::types::PASSTHROUGH_DIRS`]) deletes directly
    /// from the lower (trunk) layer — wiping `.git/HEAD`, `.git/config`,
    /// and `.git/description` and leaving the user's repository
    /// unopenable by libgit2.  This was observed in the wild after an
    /// auto-resolve flow where the FUSE daemon had not fully unmounted
    /// before `destroy_overlay` ran.
    ///
    /// So before removing anything: refuse if `mount/` is still a mount
    /// point. The caller (CLI) is responsible for unmounting first; if they
    /// did not, we fail loud rather than destroy the user's VCS.
    pub fn destroy_overlay(&mut self, agent_id: &AgentId) -> Result<(), OverlayError> {
        if self.active_overlays.remove(agent_id).is_none() {
            return Err(OverlayError::NotFound(agent_id.clone()));
        }

        let overlay_root = self.phantom_dir.join("overlays").join(&agent_id.0);
        let mount_point = overlay_root.join("mount");

        if is_fuse_mount_point(&mount_point) {
            // Reinsert a minimal handle so the caller can retry after unmount
            // without losing track of the overlay's existence. The handle is
            // not reused beyond a `NotFound` check, so constructing a fresh
            // layer would be wasteful — insert nothing and let the caller
            // re-open via `restore_overlays` if they retry.
            return Err(OverlayError::Io(std::io::Error::new(
                std::io::ErrorKind::ResourceBusy,
                format!(
                    "refusing to remove overlay '{}': FUSE is still mounted at {}. \
                     Recursive removal would route unlink calls through FUSE and \
                     delete .git/HEAD and .git/config from the trunk. Unmount first.",
                    agent_id,
                    mount_point.display(),
                ),
            )));
        }

        if overlay_root.exists() {
            // Even with the mount check above, extra defense: skip the mount
            // subdir entirely — remove every other child, then rmdir the
            // (now-empty) mount dir and finally the overlay root. A non-empty
            // mount dir here means something was written AFTER the unmount
            // check, which is a race we'd rather surface than silently
            // recurse through.
            remove_overlay_root_safely(&overlay_root, &mount_point)?;
        }

        info!(agent = %agent_id, "overlay destroyed");
        Ok(())
    }

    /// List all active overlay handles.
    #[must_use]
    pub fn list_overlays(&self) -> Vec<&MountHandle> {
        self.active_overlays.values().collect()
    }

    /// Scan `.phantom/overlays/` and return agent IDs without creating layers.
    ///
    /// This is a lightweight directory scan with no whiteout loading, no
    /// directory creation, and no [`OverlayLayer`] initialization. Use for
    /// read-only listing commands that only need agent names.
    pub fn scan_agent_ids(phantom_dir: &Path) -> Result<Vec<AgentId>, OverlayError> {
        let overlays_dir = phantom_dir.join("overlays");
        if !overlays_dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut agents = Vec::new();
        for entry in fs::read_dir(&overlays_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str()
                && entry.path().join("upper").is_dir()
            {
                agents.push(AgentId(name.to_string()));
            }
        }
        Ok(agents)
    }

    /// Return the upper directory path for an agent's overlay.
    pub fn upper_dir(&self, agent_id: &AgentId) -> Result<&Path, OverlayError> {
        self.active_overlays
            .get(agent_id)
            .map(|h| h.upper_dir.as_path())
            .ok_or_else(|| OverlayError::NotFound(agent_id.clone()))
    }

    /// Notify all active overlays that trunk has advanced.
    ///
    /// Updates each layer's lower pointer so subsequent reads fall through
    /// to the new trunk state.
    pub fn notify_trunk_advanced(&self, new_trunk_path: &Path) {
        for handle in self.active_overlays.values() {
            handle.layer.update_lower(new_trunk_path.to_path_buf());
        }
        debug!(
            new_trunk = %new_trunk_path.display(),
            overlay_count = self.active_overlays.len(),
            "all overlays notified of trunk advance"
        );
    }

    /// Get a reference to the [`OverlayLayer`] for an agent (non-FUSE access).
    pub fn get_layer(&self, agent_id: &AgentId) -> Result<&OverlayLayer, OverlayError> {
        self.active_overlays
            .get(agent_id)
            .map(|h| &h.layer)
            .ok_or_else(|| OverlayError::NotFound(agent_id.clone()))
    }

    /// Clear the upper layer for an agent after successful materialization.
    ///
    /// Removes all files from the agent's upper directory so reads fall through
    /// to the now-updated trunk.
    pub fn clear_overlay(&self, agent_id: &AgentId) -> Result<(), OverlayError> {
        let layer = self.get_layer(agent_id)?;
        layer.clear_upper()
    }
}

/// Return `true` if `path` is a current mount point (FUSE or otherwise).
///
/// Reads `/proc/self/mountinfo` and compares entries against `path` and its
/// canonicalized form.  Unknown / unreadable mountinfo is treated as
/// "not a mount" — this is the conservative default for non-Linux targets
/// (which don't run FUSE at all) but the caller MUST combine this check
/// with a working unmount step; a silent false-negative would re-open the
/// VCS-corruption hole this guard closes.
fn is_fuse_mount_point(path: &Path) -> bool {
    // Fast path: if the directory does not exist, it cannot be a mount.
    if !path.exists() {
        return false;
    }

    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        // Non-Linux or restricted environment — we cannot determine mount
        // state. Fall back to the device-id trick below.
        return is_mount_point_by_device_id(path);
    };

    let canon = path.canonicalize().ok();
    for line in mountinfo.lines() {
        // Format: `<mount_id> <parent_id> <major:minor> <root> <mount_point> ...`
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let mp = Path::new(fields[4]);
        if mp == path {
            return true;
        }
        if let Some(ref c) = canon
            && mp == c.as_path()
        {
            return true;
        }
    }
    false
}

/// Fallback mount-point detection: a directory is a mount point iff its
/// device id differs from its parent's device id.  Less reliable than
/// `/proc/mountinfo` (e.g. bind mounts within the same filesystem do not
/// change the device id) but good enough as a last-resort guard on
/// platforms without `/proc/self/mountinfo`.
fn is_mount_point_by_device_id(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(path_meta) = fs::metadata(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent_meta) = fs::metadata(parent) else {
        return false;
    };
    path_meta.dev() != parent_meta.dev()
}

/// Recursively remove every child of `overlay_root` EXCEPT the `mount/`
/// subdirectory, then rmdir `mount/` (which must be empty after unmount)
/// and finally the overlay root itself.
///
/// Guarantees that even if `mount/` is somehow a stale FUSE mount that
/// slipped past [`is_fuse_mount_point`], we never recurse into it.
fn remove_overlay_root_safely(
    overlay_root: &Path,
    mount_point: &Path,
) -> Result<(), OverlayError> {
    if !overlay_root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(overlay_root)? {
        let entry = entry?;
        let path = entry.path();
        if path == mount_point {
            // Never recurse into the mount. Just try to rmdir it (only works
            // if it's actually empty, i.e. not a live FUSE mount). Ignore
            // errors — a non-empty mount means we still shouldn't proceed.
            let _ = fs::remove_dir(&path);
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }

    // Attempt final rmdir of the overlay root. If `mount/` could not be
    // removed (still mounted or non-empty), this will fail with ENOTEMPTY,
    // which we surface rather than swallow.
    fs::remove_dir(overlay_root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn create_and_list_overlay() {
        let phantom_dir = TempDir::new().unwrap();
        let trunk_dir = TempDir::new().unwrap();

        let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
        let agent = AgentId("agent-a".into());

        mgr.create_overlay(agent.clone(), trunk_dir.path()).unwrap();

        assert_eq!(mgr.list_overlays().len(), 1);
        assert!(mgr.upper_dir(&agent).is_ok());
    }

    #[test]
    fn duplicate_overlay_errors() {
        let phantom_dir = TempDir::new().unwrap();
        let trunk_dir = TempDir::new().unwrap();

        let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
        let agent = AgentId("agent-a".into());

        mgr.create_overlay(agent.clone(), trunk_dir.path()).unwrap();
        let err = mgr.create_overlay(agent, trunk_dir.path());
        assert!(err.is_err());
    }

    #[test]
    fn destroy_overlay_removes_from_list() {
        let phantom_dir = TempDir::new().unwrap();
        let trunk_dir = TempDir::new().unwrap();

        let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
        let agent = AgentId("agent-b".into());

        mgr.create_overlay(agent.clone(), trunk_dir.path()).unwrap();
        mgr.destroy_overlay(&agent).unwrap();

        assert!(mgr.list_overlays().is_empty());
        assert!(mgr.get_layer(&agent).is_err());
    }

    #[test]
    fn destroy_nonexistent_overlay_errors() {
        let phantom_dir = TempDir::new().unwrap();
        let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
        let agent = AgentId("ghost".into());
        assert!(mgr.destroy_overlay(&agent).is_err());
    }

    #[test]
    fn get_layer_allows_read_write() {
        let phantom_dir = TempDir::new().unwrap();
        let trunk_dir = TempDir::new().unwrap();
        fs::write(trunk_dir.path().join("trunk.txt"), b"hello").unwrap();

        let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
        let agent = AgentId("agent-rw".into());
        mgr.create_overlay(agent.clone(), trunk_dir.path()).unwrap();

        // Read through layer.
        let layer = mgr.get_layer(&agent).unwrap();
        let data = layer.read_file(Path::new("trunk.txt")).unwrap();
        assert_eq!(data, b"hello");

        // Write via layer (interior mutability — no &mut needed).
        let layer = mgr.get_layer(&agent).unwrap();
        layer
            .write_file(Path::new("new.txt"), b"agent wrote this")
            .unwrap();
        layer.remove_whiteout(Path::new("new.txt"));

        let layer = mgr.get_layer(&agent).unwrap();
        let data = layer.read_file(Path::new("new.txt")).unwrap();
        assert_eq!(data, b"agent wrote this");
    }

    #[test]
    fn notify_trunk_advanced_updates_lower() {
        let phantom_dir = TempDir::new().unwrap();
        let trunk1 = TempDir::new().unwrap();
        let trunk2 = TempDir::new().unwrap();

        fs::write(trunk1.path().join("v1.txt"), b"version 1").unwrap();
        fs::write(trunk2.path().join("v2.txt"), b"version 2").unwrap();

        let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
        let agent = AgentId("agent-trunk".into());
        mgr.create_overlay(agent.clone(), trunk1.path()).unwrap();

        let layer = mgr.get_layer(&agent).unwrap();
        assert!(layer.exists(Path::new("v1.txt")));
        assert!(!layer.exists(Path::new("v2.txt")));

        mgr.notify_trunk_advanced(trunk2.path());

        let layer = mgr.get_layer(&agent).unwrap();
        assert!(!layer.exists(Path::new("v1.txt")));
        assert!(layer.exists(Path::new("v2.txt")));
    }
}
