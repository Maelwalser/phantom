//! Low-level filesystem helpers shared across the [`OverlayLayer`] submodules.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::warn;

use crate::error::OverlayError;

use super::OverlayLayer;

/// Monotonically increasing counter so concurrent `atomic_write` calls
/// inside the same process never pick the same temporary filename.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Atomically write `data` to `target` via a temporary sibling file.
///
/// On Unix, `rename` within the same filesystem is atomic, so readers never
/// see a partially-written file. The temp filename embeds both the PID and
/// a process-local sequence number so concurrent FUSE operations inside the
/// same daemon cannot race on a shared temp path.
pub(super) fn atomic_write(target: &Path, data: &[u8]) -> Result<(), OverlayError> {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = target.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
    if let Err(e) = fs::write(&tmp, data) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = fs::rename(&tmp, target) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
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
