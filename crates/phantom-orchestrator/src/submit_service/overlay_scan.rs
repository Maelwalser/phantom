//! Enumerate the agent's overlay changes (modified + deleted files),
//! filtering out anything `.gitignore` would exclude.

use std::path::PathBuf;

use phantom_overlay::OverlayLayer;

use crate::error::OrchestratorError;
use crate::git::GitOps;

/// Modified and deleted file lists after `.gitignore` filtering.
pub(super) struct OverlayChanges {
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

impl OverlayChanges {
    pub(super) fn is_empty(&self) -> bool {
        self.modified.is_empty() && self.deleted.is_empty()
    }
}

/// Collect the overlay's modified and deleted files, filtered by `.gitignore`.
///
/// Logs an `info` line when any files are filtered out so operators can notice
/// unexpectedly ignored paths.
pub(super) fn collect_changes(
    git: &GitOps,
    layer: &OverlayLayer,
) -> Result<OverlayChanges, OrchestratorError> {
    let all_modified = layer
        .modified_files()
        .map_err(|e| OrchestratorError::Overlay(e.to_string()))?;

    let deleted: Vec<PathBuf> = layer
        .deleted_files()
        .into_iter()
        .filter(|path| !git.is_ignored(path).unwrap_or(false))
        .collect();

    let total_count = all_modified.len();
    let modified: Vec<PathBuf> = all_modified
        .into_iter()
        .filter(|path| !git.is_ignored(path).unwrap_or(false))
        .collect();

    let ignored_count = total_count - modified.len();
    if ignored_count > 0 {
        tracing::info!(
            ignored_count,
            "filtered {ignored_count} gitignored file(s) from changeset"
        );
    }

    Ok(OverlayChanges { modified, deleted })
}
