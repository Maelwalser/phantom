//! Pre-submit outbound warnings.
//!
//! Before materialization, scan the submitting agent's semantic operations
//! for signature changes and deletions. For each such op, cross-reference
//! every *other* active agent's upper-layer symbols: if another agent holds
//! a reference to the symbol about to change, append a warning into the
//! submitter's `.phantom-task.md`.
//!
//! This is strictly informational — submit is never blocked. The goal is to
//! give the submitter a chance to notice coordination issues before the
//! ripple notifies the downstream agents.

use std::path::Path;

use tracing::warn;

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::AgentId;
use phantom_core::notification::DependencyImpact;
use phantom_core::traits::SemanticAnalyzer;

use crate::impact::{collect_agent_footprint, compute_impacts};
use crate::materialization_service::ActiveOverlay;
use crate::trunk_update;

/// A breaking change surfaced by the pre-submit scan.
#[derive(Debug, Clone)]
pub(super) struct PreSubmitImpact {
    pub affected_agent: AgentId,
    pub impacts: Vec<DependencyImpact>,
}

/// Run the pre-submit scan and append a warning to the submitting agent's
/// context file if any breaking changes would hit other active agents.
///
/// Silently no-ops when no breaking impacts are found — we do not want to
/// pollute prompt context with an "everything is fine" section on every
/// submit.
pub(super) fn run(
    analyzer: &dyn SemanticAnalyzer,
    submitting_agent: &AgentId,
    submitter_upper: &Path,
    operations: &[SemanticOperation],
    active_overlays: &[ActiveOverlay],
) {
    let breaking_ops = filter_breaking_operations(operations);
    if breaking_ops.is_empty() {
        return;
    }

    let per_agent =
        collect_impacts_per_agent(analyzer, submitting_agent, active_overlays, &breaking_ops);
    if per_agent.is_empty() {
        return;
    }

    let md = render_warning(&breaking_ops, &per_agent);
    if let Err(e) = trunk_update::write_trunk_update_md(submitter_upper, &md) {
        warn!(
            agent_id = %submitting_agent,
            error = %e,
            "failed to write pre-submit warning",
        );
    }
}

/// Filter to operations that represent a potentially breaking change for
/// dependents: signature modifications and deletions.
fn filter_breaking_operations(operations: &[SemanticOperation]) -> Vec<SemanticOperation> {
    operations
        .iter()
        .filter(|op| match op {
            SemanticOperation::ModifySymbol { .. } => op.is_signature_change(),
            SemanticOperation::DeleteSymbol { .. } => true,
            _ => false,
        })
        .cloned()
        .collect()
}

fn collect_impacts_per_agent(
    analyzer: &dyn SemanticAnalyzer,
    submitting_agent: &AgentId,
    active_overlays: &[ActiveOverlay],
    breaking_ops: &[SemanticOperation],
) -> Vec<PreSubmitImpact> {
    let mut out: Vec<PreSubmitImpact> = Vec::new();
    for overlay in active_overlays {
        if &overlay.agent_id == submitting_agent {
            continue;
        }
        if overlay.files_touched.is_empty() {
            continue;
        }
        let footprint =
            collect_agent_footprint(analyzer, &overlay.upper_dir, &overlay.files_touched);
        if footprint.is_empty() {
            continue;
        }
        let impacts = compute_impacts(breaking_ops, &footprint);
        if !impacts.is_empty() {
            out.push(PreSubmitImpact {
                affected_agent: overlay.agent_id.clone(),
                impacts,
            });
        }
    }
    out
}

fn render_warning(breaking_ops: &[SemanticOperation], per_agent: &[PreSubmitImpact]) -> String {
    use std::fmt::Write;

    let mut md = String::from("# Pre-Submit Dependency Warning\n\n");
    let _ = writeln!(
        md,
        "Your changeset contains {} potentially breaking change(s) for other active agents:",
        breaking_ops.len()
    );

    for op in breaking_ops {
        let name = op.symbol_name().unwrap_or("(unknown)");
        let (kind, detail) = match op {
            SemanticOperation::ModifySymbol { new_entry, .. } => {
                ("signature changed", new_entry.kind.to_string())
            }
            SemanticOperation::DeleteSymbol { id, .. } => (
                "deleted",
                id.0.split("::").last().unwrap_or("symbol").to_string(),
            ),
            _ => ("changed", String::new()),
        };
        if detail.is_empty() {
            let _ = writeln!(md, "- `{name}` ({kind})");
        } else {
            let _ = writeln!(md, "- `{name}` ({kind}, {detail})");
        }
    }

    md.push_str("\n## Affected agents\n\n");
    for entry in per_agent {
        let _ = writeln!(
            md,
            "- Agent `{}` has {} dependent reference(s):",
            entry.affected_agent,
            entry.impacts.len(),
        );
        for imp in &entry.impacts {
            let (start, _end) = imp.line_range;
            let _ = writeln!(
                md,
                "  - `{}` at `{}:{start}` ({})",
                imp.your_symbol.name(),
                imp.file.display(),
                imp.edge_kind,
            );
        }
    }

    md.push_str(
        "\n---\n*These agents will receive impact notifications automatically \
         after materialization. Submit is not blocked.*\n",
    );

    md
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use phantom_core::conflict::MergeResult;
    use phantom_core::error::CoreError;
    use phantom_core::id::{ContentHash, SymbolId};
    use phantom_core::symbol::{ReferenceKind, SymbolEntry, SymbolKind, SymbolReference};

    use super::*;

    struct FakeAnalyzer {
        syms: Vec<SymbolEntry>,
        refs: Vec<SymbolReference>,
    }

    impl SemanticAnalyzer for FakeAnalyzer {
        fn extract_symbols(
            &self,
            _path: &Path,
            _content: &[u8],
        ) -> Result<Vec<SymbolEntry>, CoreError> {
            Ok(self.syms.clone())
        }

        fn extract_references(
            &self,
            _path: &Path,
            _content: &[u8],
            _symbols: &[SymbolEntry],
        ) -> Result<Vec<SymbolReference>, CoreError> {
            Ok(self.refs.clone())
        }

        fn diff_symbols(&self, _: &[SymbolEntry], _: &[SymbolEntry]) -> Vec<SemanticOperation> {
            vec![]
        }

        fn three_way_merge(
            &self,
            _: &[u8],
            _: &[u8],
            theirs: &[u8],
            _: &Path,
        ) -> Result<phantom_core::conflict::MergeReport, CoreError> {
            Ok(phantom_core::conflict::MergeReport::semantic(
                MergeResult::Clean(theirs.to_vec()),
            ))
        }
    }

    fn build_modify_op(sig_changed: bool) -> SemanticOperation {
        let new_entry = SymbolEntry {
            id: SymbolId("crate::auth::login::function".into()),
            kind: SymbolKind::Function,
            name: "login".into(),
            scope: "crate::auth".into(),
            file: PathBuf::from("src/auth.rs"),
            byte_range: 0..10,
            content_hash: ContentHash::from_bytes(b"new"),
            signature_hash: if sig_changed {
                ContentHash::from_bytes(b"new_sig")
            } else {
                ContentHash::from_bytes(b"sig")
            },
        };
        SemanticOperation::ModifySymbol {
            file: PathBuf::from("src/auth.rs"),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: ContentHash::from_bytes(b"sig"),
            new_entry,
        }
    }

    #[test]
    fn filter_keeps_signature_changes_and_deletions() {
        let ops = vec![
            build_modify_op(true),  // signature change — kept
            build_modify_op(false), // body only — dropped
            SemanticOperation::DeleteSymbol {
                file: PathBuf::from("x.rs"),
                id: SymbolId("crate::x::function".into()),
            },
            SemanticOperation::AddSymbol {
                file: PathBuf::from("y.rs"),
                symbol: SymbolEntry {
                    id: SymbolId("crate::y::function".into()),
                    kind: SymbolKind::Function,
                    name: "y".into(),
                    scope: "crate".into(),
                    file: PathBuf::from("y.rs"),
                    byte_range: 0..5,
                    content_hash: ContentHash::zero(),
                    signature_hash: ContentHash::zero(),
                },
            }, // addition — dropped
        ];
        let kept = filter_breaking_operations(&ops);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn writes_warning_when_other_agent_depends() {
        // Submitter performs a signature change on `login`.
        // Other agent (agent-b) has a file that calls `login`.
        let dir = tempfile::tempdir().unwrap();
        let submitter_upper = dir.path().join("submitter_upper");
        let other_upper = dir.path().join("other_upper");
        std::fs::create_dir(&submitter_upper).unwrap();
        std::fs::create_dir(&other_upper).unwrap();
        // Create the context file so the warning appends into it.
        let caller_file = other_upper.join("src/caller.rs");
        std::fs::create_dir_all(caller_file.parent().unwrap()).unwrap();
        std::fs::write(&caller_file, b"fn caller() { login(); }").unwrap();
        std::fs::write(
            submitter_upper.join(".phantom-task.md"),
            "# Preamble\n\n---\n\n## Trunk Updates\n",
        )
        .unwrap();

        let analyzer = FakeAnalyzer {
            syms: vec![],
            refs: vec![SymbolReference {
                source: SymbolId("crate::caller::function".into()),
                target_name: "login".into(),
                target_scope_hint: Some("crate::auth".into()),
                kind: ReferenceKind::Call,
                file: PathBuf::from("src/caller.rs"),
                byte_range: 14..19,
            }],
        };

        let submitting = AgentId("agent-a".into());
        let other = AgentId("agent-b".into());
        let overlays = vec![
            ActiveOverlay {
                agent_id: submitting.clone(),
                files_touched: vec![PathBuf::from("src/auth.rs")],
                upper_dir: submitter_upper.clone(),
            },
            ActiveOverlay {
                agent_id: other,
                files_touched: vec![PathBuf::from("src/caller.rs")],
                upper_dir: other_upper,
            },
        ];
        let ops = vec![build_modify_op(true)];

        run(&analyzer, &submitting, &submitter_upper, &ops, &overlays);

        let ctx = std::fs::read_to_string(submitter_upper.join(".phantom-task.md")).unwrap();
        assert!(
            ctx.contains("Pre-Submit Dependency Warning"),
            "expected warning in context file: {ctx}"
        );
        assert!(ctx.contains("agent-b"), "expected agent-b mention");
        assert!(ctx.contains("`login`"), "expected login symbol mention");
    }

    #[test]
    fn no_warning_when_no_breaking_ops() {
        let dir = tempfile::tempdir().unwrap();
        let submitter_upper = dir.path().join("upper");
        std::fs::create_dir(&submitter_upper).unwrap();

        let analyzer = FakeAnalyzer {
            syms: vec![],
            refs: vec![],
        };
        let submitting = AgentId("agent-a".into());
        let ops = vec![build_modify_op(false)]; // body only
        run(&analyzer, &submitting, &submitter_upper, &ops, &[]);

        assert!(!submitter_upper.join(".phantom-trunk-update.md").exists());
    }

    #[test]
    fn no_warning_when_no_other_agent_affected() {
        let dir = tempfile::tempdir().unwrap();
        let submitter_upper = dir.path().join("submitter_upper");
        let other_upper = dir.path().join("other_upper");
        std::fs::create_dir(&submitter_upper).unwrap();
        std::fs::create_dir(&other_upper).unwrap();
        std::fs::write(other_upper.join("unrelated.rs"), b"fn unrelated() {}").unwrap();

        let analyzer = FakeAnalyzer {
            syms: vec![],
            refs: vec![], // no references → no impact
        };
        let submitting = AgentId("agent-a".into());
        let other = AgentId("agent-b".into());
        let overlays = vec![
            ActiveOverlay {
                agent_id: submitting.clone(),
                files_touched: vec![PathBuf::from("src/auth.rs")],
                upper_dir: submitter_upper.clone(),
            },
            ActiveOverlay {
                agent_id: other,
                files_touched: vec![PathBuf::from("unrelated.rs")],
                upper_dir: other_upper,
            },
        ];
        let ops = vec![build_modify_op(true)];

        run(&analyzer, &submitting, &submitter_upper, &ops, &overlays);

        assert!(!submitter_upper.join(".phantom-trunk-update.md").exists());
    }
}
