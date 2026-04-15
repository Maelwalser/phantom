//! Whiteout persistence for the copy-on-write overlay.
//!
//! Whiteouts track files that have been deleted from the overlay view but
//! still exist in the lower (trunk) layer. The set is persisted as JSON
//! in the upper layer directory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
    Ok(ws.paths.into_iter().map(PathBuf::from).collect())
}
