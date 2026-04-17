//! Path classification through the overlay layers.
//!
//! Centralizes the hidden → passthrough → whiteout → upper → lower decision
//! tree so read/write operations do not repeat it.

use std::fs;
use std::path::{Path, PathBuf};

use crate::types::{is_hidden, is_passthrough};

use super::OverlayLayer;

/// Result of classifying a relative path through the overlay layers.
///
/// Encodes the hidden → passthrough → whiteout → upper → lower decision tree
/// in a single place so callers don't repeat it.
pub(super) enum ResolvedPath {
    /// File exists in the upper (agent write) layer.
    Upper(PathBuf),
    /// File exists only in the lower (trunk) layer.
    Lower(PathBuf),
    /// Passthrough path — routed directly to the lower layer, bypassing COW.
    Passthrough(PathBuf),
    /// Path is hidden, whited-out, or absent from both layers.
    NotFound,
}

impl OverlayLayer {
    /// Classify a relative path through the overlay layers.
    ///
    /// Encodes the shared decision tree: hidden paths → `NotFound`,
    /// passthrough paths → `Passthrough`, whiteout'd paths → `NotFound`,
    /// then upper-before-lower fallback.
    pub(super) fn classify(&self, rel_path: &Path) -> ResolvedPath {
        if is_hidden(rel_path) {
            return ResolvedPath::NotFound;
        }

        let lower = self.lower_path();

        if is_passthrough(rel_path) {
            let lower_path = lower.join(rel_path);
            return if lower_path.exists() {
                ResolvedPath::Passthrough(lower_path)
            } else {
                ResolvedPath::NotFound
            };
        }

        if self.whiteouts.read().unwrap().contains(rel_path) {
            return ResolvedPath::NotFound;
        }

        let upper_path = self.upper.join(rel_path);
        if fs::symlink_metadata(&upper_path).is_ok() {
            return ResolvedPath::Upper(upper_path);
        }

        let lower_path = lower.join(rel_path);
        if fs::symlink_metadata(&lower_path).is_ok() {
            return ResolvedPath::Lower(lower_path);
        }

        ResolvedPath::NotFound
    }
}
