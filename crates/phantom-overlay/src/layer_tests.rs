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
