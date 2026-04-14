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
