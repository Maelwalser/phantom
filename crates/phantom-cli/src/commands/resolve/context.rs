//! Conflict-context helpers: token cost estimation and per-file grouping
//! used to decide between single-agent and parallel resolve strategies.

use phantom_session::context_file::ResolveConflictContext;

/// Maximum estimated token cost for a single resolve agent.
/// Beyond this, split into parallel agents (one per file).
/// ~30k tokens of conflict context leaves headroom for system prompt + reasoning.
pub(super) const SINGLE_AGENT_TOKEN_BUDGET: usize = 30_000;

/// Bytes-per-token estimate (matches `phantom-session`'s `BYTES_PER_TOKEN_ESTIMATE`).
const BYTES_PER_TOKEN: usize = 4;

/// Estimate total token cost across all conflict groups.
///
/// Sums byte lengths of all three versions (base, ours, theirs) across every
/// conflict context and divides by bytes-per-token. This gives a rough upper
/// bound on the context window cost for resolving all conflicts in one agent.
pub(super) fn estimate_conflict_tokens(groups: &[Vec<ResolveConflictContext>]) -> usize {
    let total_bytes: usize = groups
        .iter()
        .flatten()
        .map(|ctx| {
            ctx.base_content.as_ref().map_or(0, String::len)
                + ctx.ours_content.as_ref().map_or(0, String::len)
                + ctx.theirs_content.as_ref().map_or(0, String::len)
        })
        .sum();
    total_bytes / BYTES_PER_TOKEN
}

/// Group conflicts by file path so independent files can be resolved in parallel.
pub(super) fn group_conflicts_by_file(
    contexts: Vec<ResolveConflictContext>,
) -> Vec<Vec<ResolveConflictContext>> {
    use std::collections::BTreeMap;
    let mut by_file: BTreeMap<std::path::PathBuf, Vec<ResolveConflictContext>> = BTreeMap::new();
    for ctx in contexts {
        by_file
            .entry(ctx.detail.file.clone())
            .or_default()
            .push(ctx);
    }
    by_file.into_values().collect()
}
