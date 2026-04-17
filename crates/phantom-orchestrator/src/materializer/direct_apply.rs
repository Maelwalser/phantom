//! Fast-path materialization: trunk hasn't moved since the changeset was
//! created, so the overlay can be committed without a three-way merge.

use std::path::Path;

use tracing::debug;

use phantom_core::changeset::Changeset;
use phantom_core::id::GitOid;
use phantom_core::traits::EventStore;

use crate::error::OrchestratorError;
use crate::git::{self, GitOps};

use super::commit;
use super::events;
use super::MaterializeResult;

/// Read overlay files and commit via the git object database (blobs → tree →
/// commit), eliminating the TOCTOU window of the old working-tree-based flow.
pub(super) async fn direct_apply(
    git: &GitOps,
    changeset: &Changeset,
    upper_dir: &Path,
    head: &GitOid,
    message: &str,
    event_store: &dyn EventStore,
) -> Result<MaterializeResult, OrchestratorError> {
    debug!(changeset = %changeset.id, "direct apply — trunk has not advanced");

    let file_oids = git::create_blobs_from_overlay(git.repo(), upper_dir)?;
    let new_commit =
        commit::commit_from_oids(git, &file_oids, head, message, &changeset.agent_id.0)?;

    // Update working tree to match the new commit (best-effort).
    if let Err(e) = git
        .repo()
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
    {
        debug!(error = %e, "checkout_head after direct apply failed (non-fatal)");
    }

    events::finalize_with_rollback(git, event_store, changeset, head, &new_commit).await?;

    Ok(MaterializeResult::Success {
        new_commit,
        text_fallback_files: vec![],
    })
}
