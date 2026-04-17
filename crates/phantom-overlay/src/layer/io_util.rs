//! Low-level filesystem helpers shared across the [`OverlayLayer`] submodules.

use std::fs;
use std::path::Path;

use tracing::warn;

use crate::error::OverlayError;

use super::OverlayLayer;

/// Atomically write `data` to `target` via a temporary sibling file.
///
/// On Unix, `rename` within the same filesystem is atomic, so readers never
/// see a partially-written file.
pub(super) fn atomic_write(target: &Path, data: &[u8]) -> Result<(), OverlayError> {
    let tmp = target.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, target)?;
    Ok(())
}

/// Ensure the parent directory of `path` exists, creating it (and any
/// intermediate directories) if necessary.
///
/// Dedupes the `if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }`
/// pattern that appears before most upper-layer writes.
pub(super) fn ensure_parent_dir(path: &Path) -> Result<(), OverlayError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

impl OverlayLayer {
    /// Persist the whiteout set, logging a warning on failure.
    ///
    /// Used in code paths where the primary operation has already succeeded
    /// and whiteout persistence is a best-effort follow-up.
    pub(super) fn persist_whiteouts_or_warn(&self) {
        if let Err(e) = self.persist_whiteouts() {
            warn!(error = %e, "failed to persist whiteouts (best-effort)");
        }
    }
}
