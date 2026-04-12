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
    pub fn destroy_overlay(&mut self, agent_id: &AgentId) -> Result<(), OverlayError> {
        if self.active_overlays.remove(agent_id).is_none() {
            return Err(OverlayError::NotFound(agent_id.clone()));
        }

        let overlay_root = self.phantom_dir.join("overlays").join(&agent_id.0);
        if overlay_root.exists() {
            fs::remove_dir_all(&overlay_root)?;
        }

        info!(agent = %agent_id, "overlay destroyed");
        Ok(())
    }

    /// List all active overlay handles.
    #[must_use]
    pub fn list_overlays(&self) -> Vec<&MountHandle> {
        self.active_overlays.values().collect()
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
    pub fn notify_trunk_advanced(&mut self, new_trunk_path: &Path) {
        for handle in self.active_overlays.values_mut() {
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

    /// Get a mutable reference to the [`OverlayLayer`] for an agent.
    pub fn get_layer_mut(&mut self, agent_id: &AgentId) -> Result<&mut OverlayLayer, OverlayError> {
        self.active_overlays
            .get_mut(agent_id)
            .map(|h| &mut h.layer)
            .ok_or_else(|| OverlayError::NotFound(agent_id.clone()))
    }

    /// Clear the upper layer for an agent after successful materialization.
    ///
    /// Removes all files from the agent's upper directory so reads fall through
    /// to the now-updated trunk.
    pub fn clear_overlay(&mut self, agent_id: &AgentId) -> Result<(), OverlayError> {
        let layer = self.get_layer_mut(agent_id)?;
        layer.clear_upper()
    }
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

        // Write via mutable layer.
        let layer_mut = mgr.get_layer_mut(&agent).unwrap();
        layer_mut
            .write_file(Path::new("new.txt"), b"agent wrote this")
            .unwrap();
        layer_mut.remove_whiteout(Path::new("new.txt"));

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
