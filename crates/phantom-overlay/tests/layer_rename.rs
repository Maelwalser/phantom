//! Rename, chmod, and related mutation tests for [`OverlayLayer`].

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use phantom_overlay::OverlayLayer;

use common::setup;

#[test]
fn rename_file_in_upper() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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
    let (lower, upper) = setup();
    fs::create_dir_all(lower.path().join(".git/refs")).unwrap();
    fs::write(lower.path().join(".git/refs/old"), b"ref").unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    let result = layer.rename_file(Path::new("ghost.txt"), Path::new("dst.txt"));
    assert!(result.is_err());
}

#[test]
fn chmod_preserves_permissions() {
    let (lower, upper) = setup();
    fs::write(lower.path().join("script.sh"), b"#!/bin/sh\necho hi").unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    // Set executable permission.
    layer
        .set_permissions(Path::new("script.sh"), 0o755)
        .unwrap();

    // getattr should reflect the new mode.
    let meta = layer.getattr(Path::new("script.sh")).unwrap();
    let mode = meta.permissions().mode() & 0o7777;
    assert_eq!(mode, 0o755, "expected 0o755 but got {mode:#o}");

    // File should have been COW-copied to upper.
    assert!(upper.path().join("script.sh").exists());
}

#[test]
fn chmod_on_upper_file() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer.write_file(Path::new("bin"), b"binary").unwrap();
    layer.set_permissions(Path::new("bin"), 0o755).unwrap();

    let meta = layer.getattr(Path::new("bin")).unwrap();
    let mode = meta.permissions().mode() & 0o7777;
    assert_eq!(mode, 0o755);
}
