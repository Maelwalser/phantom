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

    let mut affected = RippleChecker::check_ripple(&changed_files, &active);

    // Dependency-only ripple: include every active agent whose upper-layer
    // references a symbol the changeset just changed — even if no file
    // overlaps. This is the key behaviour the semantic dependency graph
    // unlocks: without it, an agent that calls `login` but doesn't touch
    // `auth.rs` would never hear about a signature change to `login`.
    let dep_only =
        dependency_affected_agents(analyzer, &changeset.operations, active_overlays, &affected);
    for (agent_id, files) in dep_only {
        affected.entry(agent_id).or_insert(files);
    }

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

    // Skip the submitting agent — ripples never fire on the submitter itself.
    for (agent_id, files) in &affected {
        if agent_id == &changeset.agent_id {
            continue;
        }
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

/// Identify active agents whose upper-layer references target a changed
/// symbol, beyond the file-overlap set already detected by `RippleChecker`.
///
/// For each such agent, parses every file in their `files_touched` list and
/// checks for impact. The returned map uses the agent's full `files_touched`
/// as the "affected files" — downstream classification will mark each as
/// `TrunkVisible` (no live rebase needed since these files didn't change on
/// trunk; the agent just holds references to changed symbols).
fn dependency_affected_agents(
    analyzer: &dyn SemanticAnalyzer,
    operations: &[phantom_core::changeset::SemanticOperation],
    active_overlays: &[ActiveOverlay],
    already_affected: &std::collections::HashMap<AgentId, Vec<PathBuf>>,
) -> Vec<(AgentId, Vec<PathBuf>)> {
    let mut out: Vec<(AgentId, Vec<PathBuf>)> = Vec::new();
    for overlay in active_overlays {
        if already_affected.contains_key(&overlay.agent_id) {
            continue;
        }
        if overlay.files_touched.is_empty() {
            continue;
        }
        let footprint = crate::impact::collect_agent_footprint(
            analyzer,
            &overlay.upper_dir,
            &overlay.files_touched,
        );
        let impacts = crate::impact::compute_impacts(operations, &footprint);
        if !impacts.is_empty() {
            out.push((overlay.agent_id.clone(), overlay.files_touched.clone()));
        }
    }
    out
}
