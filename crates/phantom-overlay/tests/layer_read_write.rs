//! Basic read/write/merge behavior of [`OverlayLayer`].

mod common;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use phantom_overlay::OverlayLayer;
use tempfile::TempDir;

use common::setup;

#[test]
fn write_to_upper_and_read_back() {
    let (lower, upper) = setup();
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
    layer.write_file(Path::new("shared.txt"), b"upper").unwrap();

    let data = layer.read_file(Path::new("shared.txt")).unwrap();
    assert_eq!(data, b"upper");
}

#[test]
fn modified_files_returns_upper_only() {
    let (lower, upper) = setup();
    fs::write(lower.path().join("lower.txt"), b"trunk").unwrap();

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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

    let layer = OverlayLayer::new(lower1.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();
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
    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

    layer.write_file(Path::new("a/b/c.txt"), b"deep").unwrap();

    let data = layer.read_file(Path::new("a/b/c.txt")).unwrap();
    assert_eq!(data, b"deep");
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

    let layer = OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap();

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
