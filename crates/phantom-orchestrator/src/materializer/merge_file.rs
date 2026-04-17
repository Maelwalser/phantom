//! Per-file merge logic for the slow-path (`merge_apply`).
//!
//! Handles reading base/trunk/agent versions of a file, the new-file vs
//! existing-file cases, the symbol-disjoint text-merge optimization, and the
//! three-way semantic merge fallback.

use std::collections::HashSet;
use std::path::Path;

use tracing::debug;

use phantom_core::changeset::Changeset;
use phantom_core::conflict::{ConflictDetail, ConflictKind, MergeReport, MergeResult};
use phantom_core::id::ChangesetId;
use phantom_core::traits::SemanticAnalyzer;

use crate::error::OrchestratorError;
use crate::git::{GitError, GitOps};

use super::merge_apply::MergeContext;

/// Outcome of merging a single file during the merge-apply path.
pub(super) enum MergeFileOutcome {
    /// File merged cleanly with this content.
    Merged {
        content: Vec<u8>,
        text_fallback: bool,
    },
    /// File produced conflicts.
    Conflicted(Vec<ConflictDetail>),
    /// File was deleted by the agent.
    Deleted,
    /// File was skipped (not present in overlay and not a deletion).
    Skipped,
}

/// Merge a single file during the merge-apply path.
///
/// Reads the agent's version from `ctx.upper_dir`, the base version at the
/// changeset's base commit, and the trunk version at `ctx.head`, then dispatches
/// to new-file or existing-file logic.
pub(super) fn merge_single_file(
    git: &GitOps,
    file: &Path,
    changeset: &Changeset,
    ctx: &MergeContext<'_>,
    agent_file_ops: Option<&HashSet<String>>,
) -> Result<MergeFileOutcome, OrchestratorError> {
    let theirs_path = ctx.upper_dir.join(file);

    // Read agent's version from overlay. If the file doesn't exist in the
    // upper dir, it was deleted by the agent (tracked via whiteouts in the
    // overlay layer, surfaced as files_touched by submit_service).
    let theirs = if theirs_path.exists() {
        Some(std::fs::read(&theirs_path)?)
    } else {
        None
    };

    let Some(theirs) = theirs else {
        // File was deleted by the agent. Check base; if missing there too,
        // the path was never tracked — skip silently.
        match git.read_file_at_commit(&changeset.base_commit, file) {
            Ok(_) => return Ok(MergeFileOutcome::Deleted),
            Err(GitError::NotFound(_)) => return Ok(MergeFileOutcome::Skipped),
            Err(e) => return Err(e.into()),
        }
    };

    let base = match git.read_file_at_commit(&changeset.base_commit, file) {
        Ok(content) => Some(content),
        Err(GitError::NotFound(_)) => None,
        Err(e) => return Err(e.into()),
    };

    match base {
        None => merge_new_file(git, file, ctx, &theirs),
        Some(base_content) => merge_existing_file(
            git,
            file,
            changeset,
            ctx,
            &base_content,
            &theirs,
            agent_file_ops,
        ),
    }
}

/// Handle merge of a file that didn't exist at the base commit.
fn merge_new_file(
    git: &GitOps,
    file: &Path,
    ctx: &MergeContext<'_>,
    theirs: &[u8],
) -> Result<MergeFileOutcome, OrchestratorError> {
    match git.read_file_at_commit(ctx.head, file) {
        Ok(ours) => {
            // File added on trunk too — merge with empty base.
            let report = ctx
                .analyzer
                .three_way_merge(&[], &ours, theirs, file)
                .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;
            Ok(report_to_outcome(report))
        }
        Err(GitError::NotFound(_)) => Ok(MergeFileOutcome::Merged {
            content: theirs.to_vec(),
            text_fallback: false,
        }),
        Err(e) => Err(e.into()),
    }
}

/// Handle merge of a file that existed at the base commit.
fn merge_existing_file(
    git: &GitOps,
    file: &Path,
    changeset: &Changeset,
    ctx: &MergeContext<'_>,
    base_content: &[u8],
    theirs: &[u8],
    agent_file_ops: Option<&HashSet<String>>,
) -> Result<MergeFileOutcome, OrchestratorError> {
    let ours = match git.read_file_at_commit(ctx.head, file) {
        Ok(content) => content,
        Err(GitError::NotFound(_)) => {
            // File deleted on trunk since base.
            return Ok(MergeFileOutcome::Conflicted(vec![ConflictDetail {
                kind: ConflictKind::ModifyDeleteSymbol,
                file: file.to_path_buf(),
                symbol_id: None,
                ours_changeset: ChangesetId("trunk".into()),
                theirs_changeset: changeset.id.clone(),
                description: format!(
                    "file {} was deleted on trunk but modified by agent",
                    file.display()
                ),
                ours_span: None,
                theirs_span: None,
                base_span: None,
            }]));
        }
        Err(e) => return Err(e.into()),
    };

    // Trunk unchanged from base — use agent's version directly.
    if ours == base_content {
        return Ok(MergeFileOutcome::Merged {
            content: theirs.to_vec(),
            text_fallback: false,
        });
    }

    // Symbol-level disjoint check: skip expensive semantic merge when
    // agent and trunk modified different symbols.
    if symbols_disjoint(ctx.analyzer, file, base_content, &ours, agent_file_ops) {
        debug!(file = %file.display(), "symbol-disjoint — using text merge");
        if let MergeResult::Clean(content) = git.text_merge(base_content, &ours, theirs)? {
            return Ok(MergeFileOutcome::Merged {
                content,
                text_fallback: !ctx.analyzer.supports_language(file),
            });
        }
        debug!(
            file = %file.display(),
            "text merge conflict despite disjoint symbols, falling back to semantic merge"
        );
    }

    // Three-way semantic merge.
    let report = ctx
        .analyzer
        .three_way_merge(base_content, &ours, theirs, file)
        .map_err(|e| OrchestratorError::Semantic(e.to_string()))?;

    Ok(report_to_outcome(report))
}

/// Return `true` iff the agent and trunk modified disjoint symbol sets in
/// `file`, allowing the merge to skip expensive semantic analysis.
fn symbols_disjoint(
    analyzer: &dyn SemanticAnalyzer,
    file: &Path,
    base_content: &[u8],
    trunk_content: &[u8],
    agent_file_ops: Option<&HashSet<String>>,
) -> bool {
    let Some(agent_syms) = agent_file_ops else {
        return false;
    };
    if agent_syms.is_empty() {
        return false;
    }
    let base_syms = analyzer
        .extract_symbols(file, base_content)
        .unwrap_or_default();
    let trunk_syms = analyzer
        .extract_symbols(file, trunk_content)
        .unwrap_or_default();
    let trunk_ops = analyzer.diff_symbols(&base_syms, &trunk_syms);
    let trunk_names: HashSet<String> = trunk_ops
        .iter()
        .filter_map(|op| op.symbol_name().map(String::from))
        .collect();
    !agent_syms.iter().any(|s| trunk_names.contains(s.as_str()))
}

/// Convert a [`MergeReport`] into a [`MergeFileOutcome`].
///
/// The report's strategy drives the `text_fallback` flag: any
/// [`MergeStrategy`](phantom_core::conflict::MergeStrategy) variant whose
/// `is_text_fallback()` returns true is reported as a fallback so the CLI
/// can warn the operator.
fn report_to_outcome(report: MergeReport) -> MergeFileOutcome {
    let text_fallback = report.strategy.is_text_fallback();
    match report.result {
        MergeResult::Clean(content) => MergeFileOutcome::Merged {
            content,
            text_fallback,
        },
        MergeResult::Conflict(conflicts) => MergeFileOutcome::Conflicted(conflicts),
    }
}
