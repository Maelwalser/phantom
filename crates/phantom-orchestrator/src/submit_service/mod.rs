//! Submit service — extract semantic operations from an agent's overlay,
//! commit them to trunk via semantic merge, and ripple changes to other agents.
//!
//! The public entry point [`submit_and_materialize`] is a thin wrapper that
//! delegates to the internal `pipeline::run`; the actual pipeline is broken
//! up across submodules (discovery, overlay scan, operation extraction,
//! commit message, changeset builder, events).

use std::path::{Path, PathBuf};

use phantom_core::id::{AgentId, ChangesetId};
use phantom_core::traits::{EventStore, SemanticAnalyzer};
use phantom_overlay::OverlayLayer;

use crate::error::OrchestratorError;
use crate::git::GitOps;
use crate::materialization_service::{ActiveOverlay, MaterializeOutput};
use crate::materializer::Materializer;

mod changeset_builder;
mod commit_message;
mod discovery;
mod events;
mod operations;
mod overlay_scan;
mod pipeline;
mod pre_submit_warnings;
mod scope_audit;

/// Output of the submission step (semantic operation extraction).
#[derive(Debug)]
pub struct SubmitOutput {
    /// The changeset ID that was submitted.
    pub changeset_id: ChangesetId,
    /// Number of symbol additions detected.
    pub additions: u32,
    /// Number of symbol modifications detected.
    pub modifications: u32,
    /// Number of symbol deletions detected.
    pub deletions: u32,
    /// List of modified files in the overlay.
    pub modified_files: Vec<PathBuf>,
}

/// Combined output of the unified submit-and-materialize pipeline.
#[derive(Debug)]
pub struct SubmitAndMaterializeOutput {
    /// Submission stats (semantic operations extracted).
    pub submit: SubmitOutput,
    /// Materialization result (merge, commit, ripple effects).
    pub materialize: MaterializeOutput,
}

/// Submit an agent's overlay changes and materialize them to trunk in one step.
///
/// This is the unified pipeline that:
/// 1. Extracts semantic operations from each modified file.
/// 2. Runs the three-way semantic merge and commits to trunk.
/// 3. Runs ripple checking and live rebase on other active agents.
/// 4. Appends a `ChangesetSubmitted` event (H-ORC2: after step 2 succeeds).
///
/// Returns `Ok(Some(output))` if changes were found and processed, or
/// `Ok(None)` if the overlay has no modifications.
#[allow(clippy::too_many_arguments)]
pub async fn submit_and_materialize(
    git: &GitOps,
    events: &dyn EventStore,
    analyzer: &dyn SemanticAnalyzer,
    agent_id: &AgentId,
    layer: &OverlayLayer,
    upper_dir: &Path,
    phantom_dir: &Path,
    materializer: &Materializer<'_>,
    active_overlays: &[ActiveOverlay],
    message: Option<&str>,
) -> Result<Option<SubmitAndMaterializeOutput>, OrchestratorError> {
    pipeline::run(pipeline::SubmitContext {
        git,
        events,
        analyzer,
        agent_id,
        layer,
        upper_dir,
        phantom_dir,
        materializer,
        active_overlays,
        message,
    })
    .await
}
