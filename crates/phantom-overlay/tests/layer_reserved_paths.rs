//! Reserved-path write guards.
//!
//! Phantom must never let an overlay write mutate its own state
//! (`.phantom/`, `.whiteouts.json`) via the upper layer. `.git/` is handled
//! via passthrough to the lower layer (so git-aware agents can commit), but
//! every other reserved path must be rejected with
//! [`OverlayError::ReservedPath`].

mod common;

use std::path::Path;

use phantom_overlay::OverlayLayer;
use phantom_overlay::error::OverlayError;

use common::setup;

fn layer() -> OverlayLayer {
    let (lower, upper) = setup();
    let lower = Box::leak(Box::new(lower));
    let upper = Box::leak(Box::new(upper));
    OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap()
}

#[test]
fn write_to_whiteouts_json_is_rejected() {
    let layer = layer();
    let err = layer
        .write_file(Path::new(".whiteouts.json"), b"{}")
        .unwrap_err();
    assert!(
        matches!(err, OverlayError::ReservedPath(_)),
        "expected ReservedPath, got {err:?}"
    );
}

#[test]
fn write_to_nested_whiteouts_json_is_rejected() {
    let layer = layer();
    let err = layer
        .write_file(Path::new("crates/foo/.whiteouts.json"), b"{}")
        .unwrap_err();
    assert!(
        matches!(err, OverlayError::ReservedPath(_)),
        "expected ReservedPath, got {err:?}"
    );
}

#[test]
fn dotphantom_write_still_hidden_not_reserved() {
    // `.phantom/` is HIDDEN — the existing hidden-path check fires before
    // the reserved guard, returning PathNotFound. Both outcomes keep
    // Phantom state safe; this test pins the contract so a future cleanup
    // that removes the hidden-path check still produces a blocking error.
    let layer = layer();
    let err = layer
        .write_file(Path::new(".phantom/events.db"), b"corrupt")
        .unwrap_err();
    assert!(
        matches!(
            err,
            OverlayError::PathNotFound(_) | OverlayError::ReservedPath(_)
        ),
        "expected PathNotFound or ReservedPath, got {err:?}"
    );
}

#[test]
fn dotgit_writes_pass_through_to_lower() {
    // `.git/` is passthrough — writes go directly to the lower layer so
    // agents can do real git operations. This pins the contract that the
    // reserved guard does NOT break this path.
    let (lower, upper) = setup();
    let lower_path = lower.path().to_path_buf();
    std::fs::create_dir_all(lower_path.join(".git")).unwrap();
    let layer = OverlayLayer::new(lower_path.clone(), upper.path().to_path_buf()).unwrap();

    layer
        .write_file(Path::new(".git/HEAD"), b"ref: refs/heads/main\n")
        .expect(".git/ writes must succeed (passthrough)");

    // Wrote to lower, not upper.
    assert_eq!(
        std::fs::read_to_string(lower_path.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
    assert!(!upper.path().join(".git/HEAD").exists());
}

#[test]
fn rename_to_whiteouts_json_is_rejected() {
    let layer = layer();
    layer.write_file(Path::new("foo.rs"), b"").unwrap();

    let err = layer
        .rename_file(Path::new("foo.rs"), Path::new(".whiteouts.json"))
        .unwrap_err();
    assert!(
        matches!(err, OverlayError::ReservedPath(_)),
        "expected ReservedPath, got {err:?}"
    );
}

#[test]
fn symlink_targeting_dotgit_is_rejected() {
    let layer = layer();
    let err = layer
        .create_symlink(Path::new("hacked"), Path::new(".git/HEAD"))
        .unwrap_err();
    assert!(
        matches!(err, OverlayError::ReservedPath(_)),
        "expected ReservedPath, got {err:?}"
    );
}
