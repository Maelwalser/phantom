//! Notification helpers invoked after a ripple is processed.
//!
//! Writes the JSON `trunk-updated.json` marker and the `.phantom-trunk-update.md`
//! markdown summary into an affected agent's overlay, and persists the agent's
//! `current_base` so subsequent live rebases know where to start from.

use std::path::{Path, PathBuf};

use tracing::{error, warn};

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::{AgentId, GitOid};
use phantom_core::notification::{DependencyImpact, TrunkFileStatus};

use crate::live_rebase;
use crate::ripple;
use crate::trunk_update;

use super::agent_ripple::{AffectedAgent, RippleContext};

/// Write a JSON trunk notification file and update the agent's `current_base`.
///
/// Used on the no-shadowed-files path and as a fallback when live rebase fails.
pub(super) fn write_notification_and_base(
    phantom_dir: &Path,
    agent_id: &AgentId,
    head: GitOid,
    classified: Vec<(PathBuf, TrunkFileStatus)>,
    impacts: Vec<DependencyImpact>,
) {
    let notif = ripple::build_notification(head, classified, impacts);
    if let Err(e) = ripple::write_trunk_notification(phantom_dir, agent_id, &notif) {
        warn!(%agent_id, error = %e, "failed to write notification");
    }
    // Persisting current_base is essential: the agent has been notified that
    // trunk moved, and future ripples will three-way-merge against the stored
    // base. If write fails, the agent's base tracking is inconsistent with
    // reality — escalate to error so operators can investigate.
    if let Err(e) = live_rebase::write_current_base(phantom_dir, agent_id, &head) {
        error!(
            %agent_id,
            error = %e,
            "failed to update current_base after trunk notification; agent base tracking is now inconsistent"
        );
    }
}

/// Filter operations to overlapping files and write the semantic markdown
/// notification into the affected agent's upper directory.
pub(super) fn write_trunk_update(
    ctx: &RippleContext<'_>,
    target: &AffectedAgent<'_>,
    classified: &[(PathBuf, TrunkFileStatus)],
    impacts: &[DependencyImpact],
) {
    let relevant_ops: Vec<SemanticOperation> = ctx
        .operations
        .iter()
        .filter(|op| target.files.iter().any(|f| op.file_path() == f.as_path()))
        .cloned()
        .collect();

    let md = trunk_update::generate_trunk_update_md(
        ctx.submitting_agent,
        ctx.changeset_id,
        ctx.head,
        &relevant_ops,
        classified,
        impacts,
        ctx.materializer.git(),
    );
    if let Err(e) = trunk_update::write_trunk_update_md(target.upper_path, &md) {
        warn!(agent_id = %target.agent_id, error = %e, "failed to write trunk update markdown");
    }
}
