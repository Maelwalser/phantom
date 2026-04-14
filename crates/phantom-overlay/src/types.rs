//! Shared types and path classification helpers for the overlay crate.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// File type for directory entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
}

/// A directory entry returned by [`crate::layer::OverlayLayer::read_dir`].
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Entry name (file or directory name, not full path).
    pub name: OsString,
    /// The kind of filesystem entry.
    pub file_type: FileType,
}

/// Paths that are always hidden from the overlay view.
///
/// `.phantom` is the internal metadata directory (contains the FUSE mount point
/// itself — exposing it through FUSE causes a self-referential access deadlock).
pub(crate) const HIDDEN_DIRS: &[&str] = &[".phantom"];

/// Paths that bypass the COW upper layer entirely.
///
/// `.git` is passthrough so that git operations from within the overlay mount
/// (e.g. `git status`, `git commit`) read and write the real repository state.
/// All reads, writes, and metadata queries for these paths go directly to the
/// lower (trunk) layer.
pub(crate) const PASSTHROUGH_DIRS: &[&str] = &[".git"];

/// Returns `true` if a relative path starts with a hidden directory.
pub(crate) fn is_hidden(rel_path: &Path) -> bool {
    first_component(rel_path).is_some_and(|name| HIDDEN_DIRS.contains(&name))
}

/// Returns `true` if a relative path starts with a passthrough directory.
///
/// Passthrough paths are routed directly to the lower layer for all operations,
/// bypassing the upper layer and whiteout tracking.
pub(crate) fn is_passthrough(rel_path: &Path) -> bool {
    first_component(rel_path).is_some_and(|name| PASSTHROUGH_DIRS.contains(&name))
}

/// Extract the first path component as a `&str`, if possible.
pub(crate) fn first_component(rel_path: &Path) -> Option<&str> {
    rel_path
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
}

/// Collect strict child paths under `old_prefix` and compute their new paths
/// under `new_prefix`.
///
/// Returns `(old_child, new_child)` pairs. Used by both [`InodeTable::rename`]
/// and [`OverlayLayer::reconcile_whiteouts_after_rename`] to avoid duplicating
/// the child reparenting logic.
pub(crate) fn reparent_children<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
    old_prefix: &Path,
    new_prefix: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    paths
        .filter_map(|p| {
            p.strip_prefix(old_prefix).ok().and_then(|suffix| {
                if suffix.as_os_str().is_empty() {
                    None // not a strict child
                } else {
                    Some((p.clone(), new_prefix.join(suffix)))
                }
            })
        })
        .collect()
}
