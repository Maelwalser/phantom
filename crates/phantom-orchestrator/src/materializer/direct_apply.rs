//! Fast-path materialization: trunk hasn't moved since the changeset was
//! created, so the overlay can be committed without a three-way merge.

use std::path::Path;

use tracing::{debug, warn};

use phantom_core::changeset::Changeset;
use phantom_core::event::MaterializationPath;
use phantom_core::id::GitOid;
use phantom_core::traits::EventStore;

use crate::error::OrchestratorError;
use crate::git::{self, GitOps};

use super::MaterializeResult;
use super::commit;
use super::events;

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

    // Pre-commit fence (ghost-commit protocol). Append intent BEFORE any git
    // write so that a crash between commit and `ChangesetMaterialized` can
    // be reconciled against trunk HEAD. If the fence append itself fails,
    // nothing has touched trunk yet — return the error and abort.
    events::append_materialization_started(
        event_store,
        changeset,
        *head,
        MaterializationPath::Direct,
    )
    .await?;

    let file_oids = git::create_blobs_from_overlay(git.repo(), upper_dir)?;
    let new_commit =
        commit::commit_from_oids(git, &file_oids, head, message, &changeset.agent_id.0)?;

    // Update working tree to match the new commit. Escalated from debug! to
    // warn!: the FUSE overlay's lower layer reads from the working tree, so
    // a diverged tree silently gives subsequent materializations wrong inputs
    // on the three-way merge path. The commit itself is safe (merges read from
    // the object DB), but every future `changed_files` / read-through falls
    // out of sync with HEAD.
    if let Err(e) = git
        .repo()
        .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
    {
        warn!(
            error = %e,
            "checkout_head after direct apply failed; working tree has diverged from HEAD"
        );
    }

    events::finalize_with_rollback(git, event_store, changeset, head, &new_commit).await?;

    Ok(MaterializeResult::Success {
        new_commit,
        text_fallback_files: vec![],
    })
}
