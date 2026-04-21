//! Regression tests for the VCS-corruption bug discovered after `ph resolve`.
//!
//! The bug: `OverlayManager::destroy_overlay` used to `fs::remove_dir_all`
//! the overlay root. If the FUSE mount at `<overlay_root>/mount/` was still
//! live, that recursive removal walked through the mount and, for passthrough
//! paths like `.git/`, routed `unlink(2)` calls to the lower (trunk) layer —
//! wiping `.git/HEAD` and `.git/config` from the user's repository and
//! leaving it unopenable.
//!
//! These tests pin the invariant: trunk files must be byte-identical before
//! and after `destroy_overlay`, regardless of whether the mount subdirectory
//! contains regular files, stale FUSE artifacts, or is empty.

use std::path::Path;

use phantom_core::AgentId;
use phantom_overlay::OverlayManager;

/// `destroy_overlay` must never delete the trunk working tree, even if the
/// overlay's `mount/` subdir ended up with content (e.g. stale FUSE
/// artifacts, or files written through a mount that wasn't properly
/// detected as a mount point).
#[test]
fn destroy_overlay_never_deletes_trunk_files() {
    let phantom_dir = tempfile::TempDir::new().unwrap();
    let trunk_dir = tempfile::TempDir::new().unwrap();
    let trunk_path = trunk_dir.path();

    // Seed trunk with a real `.git/` directory so we can assert it survives.
    std::fs::create_dir_all(trunk_path.join(".git/refs/heads")).unwrap();
    std::fs::write(trunk_path.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    std::fs::write(trunk_path.join(".git/config"), b"[core]\n").unwrap();
    std::fs::write(trunk_path.join(".git/description"), b"test repo\n").unwrap();
    std::fs::write(trunk_path.join(".git/refs/heads/main"), b"deadbeef\n").unwrap();
    std::fs::write(trunk_path.join("README.md"), b"# test\n").unwrap();

    let head_before = std::fs::read(trunk_path.join(".git/HEAD")).unwrap();
    let config_before = std::fs::read(trunk_path.join(".git/config")).unwrap();

    let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
    let agent = AgentId("agent-danger".into());
    mgr.create_overlay(agent.clone(), trunk_path).unwrap();

    // Simulate the corruption scenario: FUSE failed to unmount cleanly and
    // left files inside `mount/` that look like trunk files (passthrough
    // `.git/HEAD`, etc.). Without a real FUSE mount we cannot fully
    // reproduce the "delete through FUSE" semantics — but we CAN verify
    // that `destroy_overlay` does not recursively traverse `mount/`, and
    // therefore cannot corrupt trunk.
    let mount_path = phantom_dir
        .path()
        .join("overlays")
        .join(&agent.0)
        .join("mount");
    std::fs::create_dir_all(mount_path.join(".git")).unwrap();
    std::fs::write(
        mount_path.join(".git/HEAD"),
        b"ref: refs/heads/main\n",
    )
    .unwrap();
    std::fs::write(mount_path.join(".git/config"), b"[core]\n").unwrap();

    // destroy_overlay must either succeed (having skipped mount/) OR fail
    // loudly (declining to touch a non-empty mount). In NEITHER case may
    // it delete files from trunk.
    let _ = mgr.destroy_overlay(&agent);

    // Critical invariants:
    assert!(
        trunk_path.join(".git/HEAD").exists(),
        "trunk .git/HEAD must survive destroy_overlay"
    );
    assert!(
        trunk_path.join(".git/config").exists(),
        "trunk .git/config must survive destroy_overlay"
    );
    assert!(
        trunk_path.join(".git/description").exists(),
        "trunk .git/description must survive destroy_overlay"
    );
    assert!(trunk_path.join("README.md").exists());
    assert_eq!(
        std::fs::read(trunk_path.join(".git/HEAD")).unwrap(),
        head_before,
    );
    assert_eq!(
        std::fs::read(trunk_path.join(".git/config")).unwrap(),
        config_before,
    );
}

/// Sanity: after a clean unmount (empty `mount/`), destroy works normally.
#[test]
fn destroy_overlay_succeeds_after_clean_unmount() {
    let phantom_dir = tempfile::TempDir::new().unwrap();
    let trunk_dir = tempfile::TempDir::new().unwrap();

    let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
    let agent = AgentId("agent-clean".into());
    mgr.create_overlay(agent.clone(), trunk_dir.path()).unwrap();

    // Simulate a clean unmount: mount/ is an empty dir.
    let mount_path = phantom_dir
        .path()
        .join("overlays")
        .join(&agent.0)
        .join("mount");
    assert!(mount_path.is_dir(), "create_overlay should have made mount/");
    assert_eq!(
        std::fs::read_dir(&mount_path).unwrap().count(),
        0,
        "mount/ should be empty after a clean unmount"
    );

    mgr.destroy_overlay(&agent)
        .expect("destroy must succeed for cleanly-unmounted overlay");

    let overlay_root = phantom_dir.path().join("overlays").join(&agent.0);
    assert!(
        !overlay_root.exists(),
        "overlay root should be fully removed after destroy"
    );
}

/// If `mount/` has leftover files (simulating an unmount that missed and
/// left artifacts behind), destroy must not delete them by walking through
/// them — it must surface the error instead.
#[test]
fn destroy_overlay_refuses_when_mount_not_empty() {
    let phantom_dir = tempfile::TempDir::new().unwrap();
    let trunk_dir = tempfile::TempDir::new().unwrap();
    let trunk_path = trunk_dir.path();

    // Seed trunk with something to prove it survives.
    std::fs::write(trunk_path.join("sentinel.txt"), b"untouched").unwrap();

    let mut mgr = OverlayManager::new(phantom_dir.path().to_path_buf());
    let agent = AgentId("agent-dirty".into());
    mgr.create_overlay(agent.clone(), trunk_path).unwrap();

    let mount_path = phantom_dir
        .path()
        .join("overlays")
        .join(&agent.0)
        .join("mount");
    std::fs::write(mount_path.join("leftover.txt"), b"shouldn't recurse here").unwrap();

    // Must error (non-empty mount/ cannot be rmdir'd).
    let result = mgr.destroy_overlay(&agent);
    assert!(
        result.is_err(),
        "destroy must not silently succeed when mount/ has content"
    );

    // Trunk sentinel still present.
    assert_eq!(
        std::fs::read(trunk_path.join("sentinel.txt")).unwrap(),
        b"untouched",
    );
}

/// `is_fuse_mount_point` sanity: a freshly-created temp directory is never
/// a mount point. `/proc` (on Linux) always is.
#[test]
fn mount_point_detection_basic() {
    // We can't call the private `is_fuse_mount_point` from outside; the
    // behavior is exercised end-to-end by `destroy_overlay_never_deletes_trunk_files`.
    // This test pins the fact that the temp dir used as a trunk in the
    // other tests is NOT reported as a mount point — if it were, our
    // tests would error out for the wrong reason.
    let tmp = tempfile::TempDir::new().unwrap();
    let meta = std::fs::metadata(tmp.path()).unwrap();
    let parent_meta = std::fs::metadata(tmp.path().parent().unwrap()).unwrap();
    use std::os::unix::fs::MetadataExt;
    // tmp and its parent are both in /tmp (usually tmpfs), same device.
    // If tempfile uses a different tmpdir strategy this could change, but
    // the invariant is: a regular subdir of /tmp is not itself a mount.
    assert_eq!(
        meta.dev(),
        parent_meta.dev(),
        "a regular tempdir subdir must share device id with its parent"
    );
    // Silence unused import warning.
    let _ = Path::new("/tmp");
}
