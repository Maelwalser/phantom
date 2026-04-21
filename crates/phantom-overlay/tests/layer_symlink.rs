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
        .create_symlink(Path::new("mylink"), Path::new("target.txt"))
        .unwrap();

    let meta = layer.getattr(Path::new("mylink")).unwrap();
    assert!(meta.is_symlink(), "getattr should report symlink file type");
}

#[test]
fn create_symlink_rejects_absolute_target() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    let err = layer
        .create_symlink(Path::new("escape"), Path::new("/etc/passwd"))
        .expect_err("absolute symlink target must be refused");
    drop(err);
    // The link must not have been written.
    assert!(
        upper.path().join("escape").symlink_metadata().is_err(),
        "rejected symlink must not leave artifacts in upper layer"
    );
}

#[test]
fn create_symlink_rejects_parentdir_escape() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer
        .create_symlink(Path::new("escape"), Path::new("../../outside"))
        .expect_err("parent-dir escape must be refused");
}

#[test]
fn create_symlink_allows_relative_target_within_subtree() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    // link at sub/link → parent depth 1 → `..` then `peer` stays inside.
    layer
        .create_symlink(Path::new("sub/link"), Path::new("../peer"))
        .expect("safe relative symlink must succeed");
}

#[test]
fn read_symlink_hides_unsafe_on_disk_target() {
    let (lower, upper) = setup();
    // Plant an unsafe absolute-target symlink directly in the upper layer,
    // bypassing `create_symlink`. Simulates out-of-band tampering.
    std::os::unix::fs::symlink("/etc/passwd", upper.path().join("planted")).unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    // read_symlink must refuse to expose the unsafe target.
    let err = layer
        .read_symlink(Path::new("planted"))
        .expect_err("unsafe planted symlink must not be exposed via read_symlink");
    drop(err);
}

#[test]
fn read_symlink_passes_through_absolute_target_in_lower_layer() {
    // Plant a symlink with an absolute target in the lower layer (the user's
    // real working tree). This mirrors how `uv venv`, `python -m venv`, and
    // similar tools create `.venv/bin/python` pointing at the system Python.
    let (lower, upper) = setup();
    std::os::unix::fs::symlink("/usr/bin/env", lower.path().join("python")).unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    let target = layer
        .read_symlink(Path::new("python"))
        .expect("absolute lower-layer symlink target must be exposed verbatim");
    assert_eq!(target, Path::new("/usr/bin/env"));
}

#[test]
fn read_symlink_passes_through_parent_escape_in_lower_layer() {
    let (lower, upper) = setup();
    // A `..`-escape from the overlay root, planted in lower.
    std::os::unix::fs::symlink("../../outside", lower.path().join("escape")).unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    let target = layer
        .read_symlink(Path::new("escape"))
        .expect("escaping lower-layer symlink target must be exposed verbatim");
    assert_eq!(target, Path::new("../../outside"));
}

#[test]
fn read_symlink_exposes_venv_python_layout_from_lower_layer() {
    // End-to-end shape of the Python venv case that motivated this fix:
    // `.venv/bin/python` pointing at an absolute interpreter path.
    let (lower, upper) = setup();
    std::fs::create_dir_all(lower.path().join(".venv/bin")).unwrap();
    std::os::unix::fs::symlink("/usr/bin/env", lower.path().join(".venv/bin/python")).unwrap();
    std::os::unix::fs::symlink("python", lower.path().join(".venv/bin/python3")).unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    assert_eq!(
        layer.read_symlink(Path::new(".venv/bin/python")).unwrap(),
        Path::new("/usr/bin/env"),
    );
    assert_eq!(
        layer.read_symlink(Path::new(".venv/bin/python3")).unwrap(),
        Path::new("python"),
    );
}

#[test]
fn rename_copyup_skips_unsafe_symlink_from_lower() {
    // When a directory lives only in the lower layer and is renamed, the
    // overlay copies it up to the upper layer at the new name.  That
    // copy-up path is the one that creates *new* symlinks on disk, so
    // unsafe targets planted in lower must be filtered out.
    let (lower, upper) = setup();
    std::fs::create_dir_all(lower.path().join("src")).unwrap();
    std::fs::write(lower.path().join("src/keep.txt"), b"ok").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", lower.path().join("src/evil")).unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer
        .rename_file(Path::new("src"), Path::new("src2"))
        .expect("rename must not fail just because a child is unsafe");

    assert!(
        upper.path().join("src2/keep.txt").exists(),
        "safe sibling should have been copied up"
    );
    assert!(
        upper.path().join("src2/evil").symlink_metadata().is_err(),
        "unsafe planted symlink must not be re-created at the new path"
    );
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

    // `bin/.bin/esbuild` avoids the built-in `node_modules` artifact exclusion;
    // this test is about symlinks, not about any particular path.
    layer
        .create_symlink(
            Path::new("bin/.bin/esbuild"),
            Path::new("../esbuild/bin/esbuild"),
        )
        .unwrap();

    let modified = layer.modified_files().unwrap();
    assert!(
        modified.iter().any(|p| p.ends_with("esbuild")),
        "modified_files should include symlinks: {modified:?}"
    );
}
