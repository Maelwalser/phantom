//! Delete, whiteout, and clear-upper behavior of [`OverlayLayer`].

mod common;

use std::fs;
use std::path::Path;

use phantom_overlay::OverlayLayer;

use common::setup;

#[test]
fn delete_hides_lower_file() {
    let (lower, upper) = setup();
    fs::write(lower.path().join("victim.txt"), b"doomed").unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
    layer.delete_file(Path::new("file.txt")).unwrap();
    assert!(!layer.exists(Path::new("file.txt")));

    layer.write_file(Path::new("file.txt"), b"v2").unwrap();

    assert!(layer.exists(Path::new("file.txt")));
    let data = layer.read_file(Path::new("file.txt")).unwrap();
    assert_eq!(data, b"v2");
}

#[test]
fn whiteout_persistence_across_instances() {
    let (lower, upper) = setup();
    fs::write(lower.path().join("persist.txt"), b"data").unwrap();

    {
        let layer =
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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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
fn git_passthrough_delete_hits_lower() {
    let (lower, upper) = setup();

    fs::create_dir_all(lower.path().join(".git")).unwrap();
    fs::write(lower.path().join(".git/MERGE_HEAD"), b"abc123").unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    // Delete a .git file — should remove from lower directly.
    layer.delete_file(Path::new(".git/MERGE_HEAD")).unwrap();
    assert!(!lower.path().join(".git/MERGE_HEAD").exists());
    // No whiteout should be created for passthrough paths.
    assert!(layer.deleted_files().is_empty());
}

#[test]
fn symlink_delete_and_whiteout() {
    let (lower, upper) = setup();

    // Create a symlink in the lower layer.
    std::os::unix::fs::symlink("target", lower.path().join("link")).unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    assert!(layer.exists(Path::new("link")));
    layer.delete_file(Path::new("link")).unwrap();
    assert!(!layer.exists(Path::new("link")));
}
