//! Shared helpers for `phantom-overlay` integration tests.

use tempfile::TempDir;

/// Create lower and upper temp dirs, returning both guards so callers can use
/// their `path()` accessors.
#[must_use]
pub fn setup() -> (TempDir, TempDir) {
    let lower = TempDir::new().unwrap();
    let upper = TempDir::new().unwrap();
    (lower, upper)
}
