//! Materialization orchestration service.
//!
//! Coordinates the full materialize-and-ripple pipeline: applies a changeset to
//! trunk, runs ripple checking on active agents, performs live rebase on
//! shadowed files, and emits audit events. The agent's overlay is intentionally
//! preserved after materialization so the session can be resumed.

use std::path::{Path, PathBuf};

use phantom_core::id::AgentId;
use phantom_core::traits::{EventStore, SemanticAnalyzer};

use crate::error::OrchestratorError;
use crate::materializer::{MaterializeResult, Materializer};
use crate::ripple::RippleChecker;

mod agent_ripple;
mod notifications;

use agent_ripple::{AffectedAgent, RippleContext, handle_agent_ripple};

/// Summary of how a ripple affected one agent.
#[derive(Debug)]
pub struct RippleEffect {
    /// The agent affected by the trunk change.
    pub agent_id: AgentId,
    /// Files that overlapped between the materialized changeset and this agent.
    pub files: Vec<PathBuf>,
    /// Number of files that were cleanly merged via live rebase.
    pub merged_count: usize,
    /// Number of files that had conflicts during live rebase.
    pub conflicted_count: usize,
}

/// Output of the full materialize-and-ripple pipeline.
#[derive(Debug)]
pub struct MaterializeOutput {
    /// The underlying materialization result (success or conflict).
    pub result: MaterializeResult,
    /// Ripple effects on active agents (empty if materialization failed or no
    /// agents were affected).
    pub ripple_effects: Vec<RippleEffect>,
}

/// Information about an active agent overlay needed for ripple checking.
pub struct ActiveOverlay {
    /// The agent's identifier.
    pub agent_id: AgentId,
    /// Files the agent has touched (from its changeset).
    pub files_touched: Vec<PathBuf>,
    /// Path to the agent's upper (write) directory.
    pub upper_dir: PathBuf,
}

/// Orchestrate the full materialize-and-ripple pipeline.
///
/// 1. Calls [`Materializer::materialize`] to commit the changeset to trunk.
/// 2. Runs ripple checking against all active agent overlays.
/// 3. For each affected agent, classifies trunk changes, attempts live rebase
///    on shadowed files, writes enriched notifications, and emits audit events.
#[allow(clippy::too_many_arguments)]
pub async fn materialize_and_ripple(
    changeset: &phantom_core::changeset::Changeset,
    upper_dir: &Path,
    events: &dyn EventStore,
    analyzer: &dyn SemanticAnalyzer,
    materializer: &Materializer<'_>,
    phantom_dir: &Path,
    active_overlays: &[ActiveOverlay],
    message: &str,
) -> Result<MaterializeOutput, OrchestratorError> {
    let result = materializer
        .materialize(
            changeset,
            upper_dir,
            events,
            analyzer,
            message,
            Some(phantom_dir),
        )
        .await?;

    let MaterializeResult::Success { .. } = &result else {
        return Ok(MaterializeOutput {
            result,
            ripple_effects: vec![],
        });
    };

    // Look up the ChangesetMaterialized event ID so LiveRebased events
    // can reference it as their causal_parent (cross-changeset DAG edge).
    let trigger_event_id = events
        .latest_event_for_changeset(&changeset.id)
        .await
        .unwrap_or(None);

    let head = materializer.git().head_oid()?;
    let changed_files = materializer
        .git()
        .changed_files(&changeset.base_commit, &head)?;

    let active: Vec<(AgentId, Vec<PathBuf>)> = active_overlays
        .iter()
        .map(|a| (a.agent_id.clone(), a.files_touched.clone()))
        .collect();

    let affected = RippleChecker::check_ripple(&changed_files, &active);
    let mut ripple_effects = Vec::new();

    let ripple_ctx = RippleContext {
        materializer,
        analyzer,
        events,
        phantom_dir,
        changeset_id: &changeset.id,
        submitting_agent: &changeset.agent_id,
        changeset_base: &changeset.base_commit,
        head: &head,
        operations: &changeset.operations,
        trigger_event_id,
    };

    for (agent_id, files) in &affected {
        let Some(overlay) = active_overlays.iter().find(|a| a.agent_id == *agent_id) else {
            continue;
        };

        let target = AffectedAgent {
            agent_id,
            files,
            upper_path: &overlay.upper_dir,
        };

        ripple_effects.push(handle_agent_ripple(&ripple_ctx, &target).await);
    }

    Ok(MaterializeOutput {
        result,
        ripple_effects,
    })
}
