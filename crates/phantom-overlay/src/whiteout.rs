//! Whiteout persistence for the copy-on-write overlay.
//!
//! Whiteouts track files that have been deleted from the overlay view but
//! still exist in the lower (trunk) layer. The set is persisted as JSON
//! in the upper layer directory.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::OverlayError;

/// Name of the whiteout persistence file in the upper directory.
pub(crate) const WHITEOUT_FILE: &str = ".whiteouts.json";

/// Internal files placed in the upper layer by Phantom that should never appear
/// as agent modifications or be committed to git.
pub(crate) const INTERNAL_FILES: &[&str] = &[
    ".whiteouts.json",
    ".phantom-task.md",
    ".phantom-trunk-update.md",
];

/// Serializable whiteout set for persistence.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct WhiteoutSet {
    pub(crate) paths: Vec<String>,
}

/// Load whiteouts from the persisted JSON file in the upper directory.
pub(crate) fn load_whiteouts(upper: &Path) -> Result<HashSet<PathBuf>, OverlayError> {
    let path = upper.join(WHITEOUT_FILE);
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let data = std::fs::read_to_string(&path)?;
    let ws: WhiteoutSet =
        serde_json::from_str(&data).map_err(|e| OverlayError::Serialization(e.to_string()))?;
    Ok(ws
        .paths
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| is_safe_relative_path(p))
        .collect())
}

/// Check that a path is safe for overlay use: must be relative with no `..` components.
pub(crate) fn is_safe_relative_path(p: &Path) -> bool {
    p.is_relative() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// Return true iff `target` is safe to write as a symlink sitting at
/// `link_rel_path` inside the overlay.
///
/// A safe target is relative and, when resolved from the link's parent
/// directory, never escapes the overlay root.  Absolute paths, drive
/// prefixes, and root components are always unsafe; `..` components are
/// safe only while a corresponding number of descending components have
/// already been consumed.
///
/// `link_rel_path` is the path of the symlink itself (relative to the
/// overlay root).  Its component count minus one is the depth of the
/// link's parent directory, which sets the budget for `..` traversals.
pub(crate) fn is_safe_symlink_target(target: &Path, link_rel_path: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth = link_rel_path
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count()
        .saturating_sub(1);
    for comp in target.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => return false,
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn absolute_target_rejected() {
        assert!(!is_safe_symlink_target(
            Path::new("/etc/passwd"),
            Path::new("link"),
        ));
    }

    #[test]
    fn simple_relative_target_ok() {
        assert!(is_safe_symlink_target(
            Path::new("sibling"),
            Path::new("link"),
        ));
    }

    #[test]
    fn curdir_prefix_ok() {
        assert!(is_safe_symlink_target(
            Path::new("./sibling"),
            Path::new("link"),
        ));
    }

    #[test]
    fn descend_into_subdir_ok() {
        assert!(is_safe_symlink_target(
            Path::new("sub/file"),
            Path::new("link"),
        ));
    }

    #[test]
    fn ascend_from_subdir_ok() {
        // link = a/b/link — parent depth is 2, so `..` (go to a/) is fine.
        assert!(is_safe_symlink_target(
            Path::new("../sibling"),
            Path::new("a/b/link"),
        ));
    }

    #[test]
    fn escape_via_parentdir_rejected() {
        // link at root depth 0; one `..` already escapes.
        assert!(!is_safe_symlink_target(
            Path::new("../outside"),
            Path::new("link"),
        ));
    }

    #[test]
    fn escape_via_deep_parentdir_rejected() {
        // link at a/link → parent depth 1.
        // sub (+1 = 2) / .. (-1 = 1) / .. (-1 = 0) / .. → REJECT.
        assert!(!is_safe_symlink_target(
            Path::new("sub/../../../outside"),
            Path::new("a/link"),
        ));
    }

    #[test]
    fn nested_traversal_staying_inside_ok() {
        // link at a/link → `sub/../../outside` lands at `outside` at the
        // overlay root, a sibling of `a/`. Stays inside.
        assert!(is_safe_symlink_target(
            Path::new("sub/../../outside"),
            Path::new("a/link"),
        ));
    }

    #[test]
    fn deep_link_with_bounded_ascent_ok() {
        // link = a/b/c/link — parent depth 3, target pops twice, re-enters.
        assert!(is_safe_symlink_target(
            Path::new("../../x/y"),
            Path::new("a/b/c/link"),
        ));
    }
}
