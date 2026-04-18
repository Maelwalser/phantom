//! Copy-on-write overlay layer.
//!
//! [`OverlayLayer`] handles all COW logic using plain filesystem operations.
//! Reads check the upper (agent write) layer first, then fall through to the
//! lower (trunk) layer. Writes always go to the upper layer. Deletes are
//! tracked via a whiteout set persisted to `upper/.whiteouts.json`.
//!
//! The implementation is split across submodules by responsibility:
//!
//! - `classify` — path routing (hidden / passthrough / whiteout / upper / lower)
//! - `read` — read-side operations (read_file, read_dir, getattr, …)
//! - `write` — write-side operations (write_file, truncate_file, delete_file, …)
//! - `rename` — rename coordination (3-phase whiteout reconciliation)
//! - `maintenance` — lifecycle utilities (clear_upper, update_lower, whiteout persistence)
//! - `io_util` — shared low-level filesystem helpers

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use tracing::debug;

use crate::error::OverlayError;
use crate::types::is_passthrough;
use crate::whiteout::load_whiteouts;

mod classify;
mod io_util;
mod maintenance;
mod read;
mod rename;
mod write;

/// Copy-on-write overlay layer.
///
/// The lower layer is the trunk working tree (read-only source of truth).
/// The upper layer is the per-agent write directory. Deleted files from the
/// lower layer are tracked in a whiteout set so reads correctly report them
/// as absent.
///
/// Uses interior mutability for thread safety: the whiteout set and lower
/// path are behind fine-grained `RwLock`s, allowing concurrent read
/// operations while mutations only briefly lock the mutable state (never
/// during slow I/O like COW copies).
pub struct OverlayLayer {
    pub(super) lower: RwLock<PathBuf>,
    pub(super) upper: PathBuf,
    pub(super) whiteouts: RwLock<HashSet<PathBuf>>,
}

impl OverlayLayer {
    /// Create a new overlay layer.
    ///
    /// Creates the upper directory if it does not already exist. Loads any
    /// previously persisted whiteouts from `upper/.whiteouts.json`.
    pub fn new(lower: PathBuf, upper: PathBuf) -> Result<Self, OverlayError> {
        fs::create_dir_all(&upper)?;

        let whiteouts = load_whiteouts(&upper)?;

        debug!(
            lower = %lower.display(),
            upper = %upper.display(),
            whiteout_count = whiteouts.len(),
            "overlay layer created"
        );

        Ok(Self {
            lower: RwLock::new(lower),
            upper,
            whiteouts: RwLock::new(whiteouts),
        })
    }

    /// Read the lower path. Helper to reduce lock boilerplate.
    pub(super) fn lower_path(&self) -> PathBuf {
        self.lower
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Return a reference to the upper directory path.
    #[must_use]
    pub fn upper_dir(&self) -> &Path {
        &self.upper
    }

    /// Return the lower directory path.
    ///
    /// Returns a clone because the lower path is behind an `RwLock` for
    /// interior mutability (needed by `update_lower`).
    #[must_use]
    pub fn lower_dir(&self) -> PathBuf {
        self.lower_path()
    }

    /// Returns `true` if the given relative path is a passthrough path.
    ///
    /// Passthrough paths bypass the COW upper layer and route directly to the
    /// lower (trunk) layer for all operations.
    #[must_use]
    pub fn is_passthrough(&self, rel_path: &Path) -> bool {
        is_passthrough(rel_path)
    }
}
