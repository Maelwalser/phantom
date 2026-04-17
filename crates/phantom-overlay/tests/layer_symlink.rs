//! Symlink creation / readdir / modified-files tests for [`OverlayLayer`].

mod common;

use std::path::Path;

use phantom_overlay::{FileType, OverlayLayer};

use common::setup;

#[test]
fn create_symlink_and_readlink() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer.write_file(Path::new("target.txt"), b"hello").unwrap();
    layer
        .create_symlink(Path::new("link.txt"), Path::new("target.txt"))
        .unwrap();

    // Symlink should exist.
    assert!(layer.exists(Path::new("link.txt")));

    // readlink should return the target.
    let target = layer.read_symlink(Path::new("link.txt")).unwrap();
    assert_eq!(target, Path::new("target.txt"));

    // Symlink lives in upper layer.
    assert!(
        upper
            .path()
            .join("link.txt")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn symlink_getattr_reports_symlink_type() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer
        .create_symlink(Path::new("mylink"), Path::new("/some/target"))
        .unwrap();

    let meta = layer.getattr(Path::new("mylink")).unwrap();
    assert!(meta.is_symlink(), "getattr should report symlink file type");
}

#[test]
fn symlink_appears_in_readdir() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer.write_file(Path::new("file.txt"), b"data").unwrap();
    layer
        .create_symlink(Path::new("link"), Path::new("file.txt"))
        .unwrap();

    let entries = layer.read_dir(Path::new("")).unwrap();
    let link_entry = entries.iter().find(|e| e.name.to_string_lossy() == "link");
    assert!(link_entry.is_some(), "symlink should appear in readdir");
    assert_eq!(link_entry.unwrap().file_type, FileType::Symlink);
}

#[test]
fn modified_files_includes_symlinks() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer
        .create_symlink(
            Path::new("node_modules/.bin/esbuild"),
            Path::new("../esbuild/bin/esbuild"),
        )
        .unwrap();

    let modified = layer.modified_files().unwrap();
    assert!(
        modified.iter().any(|p| p.ends_with("esbuild")),
        "modified_files should include symlinks: {modified:?}"
    );
}
