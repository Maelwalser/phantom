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

use super::commit;
use super::events;
use super::merge_file::{self, MergeFileOutcome};
use super::path_validation;
use super::MaterializeResult;

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
    let mut merged_files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
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
                merged_files.push((file.clone(), content));
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

    if let Err(e) = git
        .repo()
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
    {
        debug!(error = %e, "checkout_head after merge commit failed (non-fatal)");
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
