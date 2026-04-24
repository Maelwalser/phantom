//! Slow-path merge: trunk advanced since the changeset's base. Runs a
//! three-way merge per file and commits the result (or returns conflicts).

use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use phantom_core::changeset::Changeset;
use phantom_core::id::GitOid;
use phantom_core::traits::{EventStore, SemanticAnalyzer};

use crate::error::OrchestratorError;
use crate::git::{self, GitOps};
use crate::ops::group_ops_by_file;

use super::MaterializeResult;
use super::commit;
use super::events;
use super::merge_file::{self, MergeFileOutcome};
use super::path_validation;

/// Resolve the git file mode for a merged-in file.
///
/// Prefers the agent's overlay (freshest signal — if the agent chmod'd the
/// file, we want that). Falls back to the base trunk tree if the file is
/// pre-existing. Defaults to a regular non-executable file.
fn resolve_mode_for_merge(file: &Path, upper_dir: &Path, git: &GitOps, head: &GitOid) -> u32 {
    let overlay_path = upper_dir.join(file);
    if let Ok(meta) = std::fs::symlink_metadata(&overlay_path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.file_type().is_symlink() {
                return 0o120_000;
            }
            if meta.permissions().mode() & 0o111 != 0 {
                return 0o100_755;
            }
            return 0o100_644;
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            return 0o100_644;
        }
    }
    // Fall back to the mode stored on trunk.
    if let Ok(git2_head) = git::git_oid_to_oid(head)
        && let Ok(commit) = git.repo().find_commit(git2_head)
        && let Ok(tree) = commit.tree()
        && let Ok(entry) = tree.get_path(file)
    {
        return u32::try_from(entry.filemode()).unwrap_or(0o100_644);
    }
    0o100_644
}

/// Bundled context for a merge-apply operation, avoiding excessive parameter counts.
pub(super) struct MergeContext<'a> {
    pub(super) upper_dir: &'a Path,
    pub(super) trunk_path: &'a Path,
    pub(super) head: &'a GitOid,
    pub(super) message: &'a str,
    pub(super) event_store: &'a dyn EventStore,
    pub(super) analyzer: &'a dyn SemanticAnalyzer,
}

/// Execute the slow-path merge, commit the result, and emit audit events.
pub(super) async fn merge_apply(
    git: &GitOps,
    changeset: &Changeset,
    ctx: &MergeContext<'_>,
) -> Result<MaterializeResult, OrchestratorError> {
    debug!(
        changeset = %changeset.id,
        base = %changeset.base_commit,
        head = %ctx.head,
        "trunk advanced — running semantic merge"
    );

    let mut all_conflicts = Vec::new();
    let mut merged_files: Vec<(PathBuf, Vec<u8>, u32)> = Vec::new();
    let mut deleted_files: Vec<PathBuf> = Vec::new();
    let mut text_fallback_files: Vec<PathBuf> = Vec::new();

    let agent_ops_by_file = group_ops_by_file(&changeset.operations);

    for file in &changeset.files_touched {
        path_validation::validate_path(file, ctx.trunk_path)?;

        let agent_file_ops = agent_ops_by_file.get(file);
        match merge_file::merge_single_file(git, file, changeset, ctx, agent_file_ops)? {
            MergeFileOutcome::Merged {
                content,
                text_fallback,
            } => {
                if text_fallback {
                    text_fallback_files.push(file.clone());
                }
                let mode = resolve_mode_for_merge(file, ctx.upper_dir, git, ctx.head);
                merged_files.push((file.clone(), content, mode));
            }
            MergeFileOutcome::Conflicted(conflicts) => {
                all_conflicts.extend(conflicts);
            }
            MergeFileOutcome::Deleted => {
                deleted_files.push(file.clone());
            }
            MergeFileOutcome::Skipped => {}
        }
    }

    if !all_conflicts.is_empty() {
        events::append_conflicted_event(ctx.event_store, changeset, &all_conflicts).await?;
        return Ok(MaterializeResult::Conflict {
            details: all_conflicts,
        });
    }

    let merged_oids = git::create_blobs_from_content(git.repo(), &merged_files)?;
    let new_commit = commit::commit_from_oids_with_deletions(
        git,
        &merged_oids,
        &deleted_files,
        ctx.head,
        ctx.message,
        &changeset.agent_id.0,
    )?;

    // Escalated from debug! to warn!: the FUSE overlay's lower layer reads
    // from the working tree. Failing to refresh it leaves subsequent
    // materializations comparing changesets against stale file content.
    if let Err(e) = git
        .repo()
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
    {
        warn!(
            error = %e,
            "checkout_head after merge commit failed; working tree has diverged from HEAD"
        );
    }

    if !text_fallback_files.is_empty() {
        warn!(
            changeset = %changeset.id,
            files = ?text_fallback_files,
            "materialized with {} file(s) merged via line-based text fallback (no syntax validation)",
            text_fallback_files.len()
        );
    }

    events::finalize_with_rollback(git, ctx.event_store, changeset, ctx.head, &new_commit).await?;

    Ok(MaterializeResult::Success {
        new_commit,
        text_fallback_files,
    })
}
