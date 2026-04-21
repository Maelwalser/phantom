//! Dependency-impact analysis for ripple delivery.
//!
//! When a changeset is materialized, the orchestrator walks each active
//! agent's upper-layer files, extracts the outbound references they hold,
//! and emits a [`DependencyImpact`] for every reference that targets a
//! symbol the submitter just changed.
//!
//! This module owns:
//! * [`collect_agent_footprint`] — parse every file in an agent's upper
//!   layer, returning its symbols + references keyed by path.
//! * [`compute_impacts`] — cross-reference an agent's footprint against a
//!   list of trunk-side semantic operations to produce the per-agent
//!   impact list that ships inside `trunk-updated.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use phantom_core::changeset::SemanticOperation;
use phantom_core::id::SymbolId;
use phantom_core::notification::{DependencyImpact, ImpactChange};
use phantom_core::symbol::{SymbolEntry, SymbolReference};
use phantom_core::traits::SemanticAnalyzer;

/// Symbols and references extracted from one of an agent's upper-layer files.
#[derive(Debug, Clone, Default)]
pub struct AgentFileFootprint {
    pub symbols: Vec<SymbolEntry>,
    pub references: Vec<SymbolReference>,
    pub content: Vec<u8>,
}

/// An agent's full semantic footprint — one [`AgentFileFootprint`] per
/// upper-layer file that was parseable by the analyzer.
///
/// Files that fail to parse are silently dropped. This is intentional: the
/// dependency graph is a best-effort signal, not a correctness boundary —
/// we'd rather produce fewer impacts than block the ripple pipeline on a
/// broken overlay.
pub type AgentFootprint = HashMap<PathBuf, AgentFileFootprint>;

/// Parse every file in an agent's upper layer that the analyzer supports
/// and return its [`AgentFileFootprint`].
///
/// `files` should be the subset of the agent's working set that overlaps
/// the materialized changeset — there's no value in parsing files the
/// submitter didn't touch. Missing files (e.g. if the agent deleted a path
/// but the ripple still lists it) are skipped.
#[must_use]
pub fn collect_agent_footprint(
    analyzer: &dyn SemanticAnalyzer,
    upper_dir: &Path,
    files: &[PathBuf],
) -> AgentFootprint {
    let mut out = AgentFootprint::new();
    for relative in files {
        let full = upper_dir.join(relative);
        let Ok(content) = std::fs::read(&full) else {
            continue;
        };
        let Ok(symbols) = analyzer.extract_symbols(relative, &content) else {
            continue;
        };
        let references = analyzer
            .extract_references(relative, &content, &symbols)
            .unwrap_or_default();
        out.insert(
            relative.clone(),
            AgentFileFootprint {
                symbols,
                references,
                content,
            },
        );
    }
    out
}

/// Index a list of semantic operations by the affected symbol's short name.
///
/// Used to answer "was name X changed on trunk?" in O(1) during impact
/// analysis, with the operation carrying enough detail to classify the
/// change severity and build a preview snippet.
struct ChangeIndex<'a> {
    by_name: HashMap<&'a str, Vec<ChangedSymbol<'a>>>,
}

struct ChangedSymbol<'a> {
    id: SymbolId,
    scope: String,
    change: ImpactChange,
    /// New symbol entry, when the change is an add or modify (`None` for
    /// deletions). Used to render a preview snippet.
    new_entry: Option<&'a SymbolEntry>,
}

impl<'a> ChangeIndex<'a> {
    fn build(operations: &'a [SemanticOperation]) -> Self {
        let mut by_name: HashMap<&'a str, Vec<ChangedSymbol<'a>>> = HashMap::new();
        for op in operations {
            let entry = match op {
                SemanticOperation::AddSymbol { symbol, .. } => ChangedSymbol {
                    id: symbol.id.clone(),
                    scope: symbol.scope.clone(),
                    change: ImpactChange::Added,
                    new_entry: Some(symbol),
                },
                SemanticOperation::ModifySymbol { new_entry, .. } => ChangedSymbol {
                    id: new_entry.id.clone(),
                    scope: new_entry.scope.clone(),
                    change: if op.is_signature_change() {
                        ImpactChange::SignatureChanged
                    } else {
                        ImpactChange::BodyOnlyChanged
                    },
                    new_entry: Some(new_entry),
                },
                SemanticOperation::DeleteSymbol { id, .. } => ChangedSymbol {
                    id: id.clone(),
                    scope: id.scope().to_string(),
                    change: ImpactChange::Deleted,
                    new_entry: None,
                },
                // File-level ops and raw diffs don't carry symbol-level info.
                _ => continue,
            };
            let name: &str = match op {
                SemanticOperation::AddSymbol { symbol, .. } => &symbol.name,
                SemanticOperation::ModifySymbol { new_entry, .. } => &new_entry.name,
                SemanticOperation::DeleteSymbol { id, .. } => id.name(),
                _ => continue,
            };
            by_name.entry(name).or_default().push(entry);
        }
        Self { by_name }
    }

    fn lookup(&self, name: &str) -> &[ChangedSymbol<'a>] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }
}

/// Compute per-agent dependency impacts.
///
/// For every reference the agent holds, check whether its target name
/// appears in `operations`. When it does, apply the same scope-match
/// heuristic the dependency graph uses and emit a [`DependencyImpact`].
///
/// The result is sorted by severity (most severe first) and then by file
/// path for stable presentation across runs.
#[must_use]
pub fn compute_impacts(
    operations: &[SemanticOperation],
    footprint: &AgentFootprint,
) -> Vec<DependencyImpact> {
    let index = ChangeIndex::build(operations);
    let mut impacts: Vec<DependencyImpact> = Vec::new();

    for (file, fp) in footprint {
        for reference in &fp.references {
            let candidates = index.lookup(&reference.target_name);
            if candidates.is_empty() {
                continue;
            }
            for changed in candidates {
                if !reference_targets_changed_symbol(reference, changed) {
                    continue;
                }
                if is_self_reference(reference, changed) {
                    continue;
                }
                let line_range = line_range(&fp.content, &reference.byte_range);
                let trunk_preview = changed.new_entry.map(preview_snippet);
                impacts.push(DependencyImpact {
                    your_symbol: reference.source.clone(),
                    depends_on: changed.id.clone(),
                    change: changed.change,
                    edge_kind: reference.kind,
                    file: file.clone(),
                    byte_range: reference.byte_range.clone(),
                    line_range,
                    trunk_preview,
                });
            }
        }
    }

    impacts.sort_by(|a, b| {
        b.change
            .severity()
            .cmp(&a.change.severity())
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.byte_range.start.cmp(&b.byte_range.start))
    });
    impacts
}

/// A reference targets a changed symbol when:
/// * The names match (ensured at the index level by the caller), AND
/// * Either the reference has no scope hint, or the hint is compatible with
///   the changed symbol's scope.
fn reference_targets_changed_symbol(
    reference: &SymbolReference,
    changed: &ChangedSymbol<'_>,
) -> bool {
    let Some(hint) = &reference.target_scope_hint else {
        return true;
    };
    scope_matches(&changed.scope, hint)
}

/// Skip the edge when the agent's local symbol and the changed trunk symbol
/// look like the same symbol — e.g. the agent just body-edited their own
/// copy of `login()` and the "reference" is a self-recursion.
///
/// We identify this case by SymbolId equality: if the enclosing source equals
/// the changed target, it's the same symbol and no impact applies.
fn is_self_reference(reference: &SymbolReference, changed: &ChangedSymbol<'_>) -> bool {
    reference.source == changed.id
}

/// Mirror of the graph's scope-match helper — kept local so this module
/// doesn't depend on `phantom-semantic`'s private items.
fn scope_matches(candidate_scope: &str, hint: &str) -> bool {
    if candidate_scope == hint {
        return true;
    }
    if candidate_scope.ends_with(hint) {
        let boundary = candidate_scope.len() - hint.len();
        return boundary == 0 || candidate_scope.as_bytes()[boundary - 1] == b':';
    }
    false
}

/// Convert a byte range into (start_line, end_line) 1-based line numbers.
///
/// Counts newlines in `content` up to `range.start` and `range.end`.
fn line_range(content: &[u8], range: &std::ops::Range<usize>) -> (u32, u32) {
    let start_line = 1 + byte_to_line_offset(content, range.start);
    let end_line = 1 + byte_to_line_offset(content, range.end.min(content.len()));
    (start_line, end_line)
}

#[allow(clippy::naive_bytecount)]
fn byte_to_line_offset(content: &[u8], pos: usize) -> u32 {
    // Naive byte counting is acceptable: this runs once per impact during
    // notification rendering and the files are source code, not binary blobs.
    let clamped = pos.min(content.len());
    u32::try_from(content[..clamped].iter().filter(|b| **b == b'\n').count()).unwrap_or(u32::MAX)
}

/// Build a short preview of a symbol's declaration for the `trunk_preview`
/// field on [`DependencyImpact`].
///
/// Currently returns a safe, stable "{kind} `{name}`" label. The full body
/// preview would require access to the trunk file content (the entry's
/// `byte_range` refers to the trunk copy, which we don't have in this
/// module); falling back to kind+name keeps the output informative without
/// misattribution.
fn preview_snippet(entry: &SymbolEntry) -> String {
    format!("{} `{}`", entry.kind, entry.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::id::{ContentHash, SymbolId};
    use phantom_core::symbol::{ReferenceKind, SymbolKind};

    fn sym(name: &str, scope: &str, kind: SymbolKind) -> SymbolEntry {
        let kind_str = format!("{kind:?}").to_lowercase();
        SymbolEntry {
            id: SymbolId(format!("{scope}::{name}::{kind_str}")),
            kind,
            name: name.into(),
            scope: scope.into(),
            file: PathBuf::from("src/foo.rs"),
            byte_range: 0..10,
            content_hash: ContentHash::from_bytes(name.as_bytes()),
            signature_hash: ContentHash::from_bytes(name.as_bytes()),
        }
    }

    fn sref(
        source: &str,
        target_name: &str,
        scope_hint: Option<&str>,
        kind: ReferenceKind,
        byte_range: std::ops::Range<usize>,
    ) -> SymbolReference {
        SymbolReference {
            source: SymbolId(source.into()),
            target_name: target_name.into(),
            target_scope_hint: scope_hint.map(str::to_string),
            kind,
            file: PathBuf::from("src/caller.rs"),
            byte_range,
        }
    }

    fn make_footprint(refs: Vec<SymbolReference>, content: &[u8]) -> AgentFootprint {
        let mut map = AgentFootprint::new();
        map.insert(
            PathBuf::from("src/caller.rs"),
            AgentFileFootprint {
                symbols: Vec::new(),
                references: refs,
                content: content.to_vec(),
            },
        );
        map
    }

    #[test]
    fn signature_change_emits_signature_changed_impact() {
        let mut new_sym = sym("login", "crate::auth", SymbolKind::Function);
        new_sym.signature_hash = ContentHash::from_bytes(b"new_sig");
        let ops = vec![SemanticOperation::ModifySymbol {
            file: PathBuf::from("src/auth.rs"),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: ContentHash::from_bytes(b"old_sig"),
            new_entry: new_sym,
        }];
        let refs = vec![sref(
            "crate::caller::function",
            "login",
            Some("crate::auth"),
            ReferenceKind::Call,
            10..15,
        )];
        let footprint = make_footprint(refs, b"line one\nline two\ncaller  login()");

        let impacts = compute_impacts(&ops, &footprint);
        assert_eq!(impacts.len(), 1);
        assert_eq!(impacts[0].change, ImpactChange::SignatureChanged);
        assert_eq!(impacts[0].edge_kind, ReferenceKind::Call);
    }

    #[test]
    fn body_only_change_emits_body_only_impact() {
        let mut new_sym = sym("login", "crate::auth", SymbolKind::Function);
        let sig = ContentHash::from_bytes(b"stable_sig");
        new_sym.signature_hash = sig;
        let ops = vec![SemanticOperation::ModifySymbol {
            file: PathBuf::from("src/auth.rs"),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: sig,
            new_entry: new_sym,
        }];
        let refs = vec![sref(
            "crate::caller::function",
            "login",
            None,
            ReferenceKind::Call,
            0..5,
        )];
        let footprint = make_footprint(refs, b"login()");

        let impacts = compute_impacts(&ops, &footprint);
        assert_eq!(impacts.len(), 1);
        assert_eq!(impacts[0].change, ImpactChange::BodyOnlyChanged);
    }

    #[test]
    fn deleted_symbol_emits_deleted_impact() {
        let ops = vec![SemanticOperation::DeleteSymbol {
            file: PathBuf::from("src/auth.rs"),
            id: SymbolId("crate::auth::login::function".into()),
        }];
        let refs = vec![sref(
            "crate::caller::function",
            "login",
            Some("crate::auth"),
            ReferenceKind::Call,
            0..5,
        )];
        let footprint = make_footprint(refs, b"login()");

        let impacts = compute_impacts(&ops, &footprint);
        assert_eq!(impacts.len(), 1);
        assert_eq!(impacts[0].change, ImpactChange::Deleted);
    }

    #[test]
    fn no_match_when_name_differs() {
        let new_sym = sym("login", "crate::auth", SymbolKind::Function);
        let ops = vec![SemanticOperation::ModifySymbol {
            file: PathBuf::from("src/auth.rs"),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: ContentHash::from_bytes(b"old_sig"),
            new_entry: new_sym,
        }];
        let refs = vec![sref(
            "crate::caller::function",
            "logout",
            None,
            ReferenceKind::Call,
            0..6,
        )];
        let footprint = make_footprint(refs, b"logout()");

        let impacts = compute_impacts(&ops, &footprint);
        assert!(impacts.is_empty());
    }

    #[test]
    fn scope_hint_mismatch_drops_impact() {
        let new_sym = sym("login", "crate::auth", SymbolKind::Function);
        let ops = vec![SemanticOperation::ModifySymbol {
            file: PathBuf::from("src/auth.rs"),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: ContentHash::from_bytes(b"old_sig"),
            new_entry: new_sym,
        }];
        let refs = vec![sref(
            "crate::caller::function",
            "login",
            Some("crate::billing"), // wrong scope
            ReferenceKind::Call,
            0..5,
        )];
        let footprint = make_footprint(refs, b"login()");

        let impacts = compute_impacts(&ops, &footprint);
        assert!(impacts.is_empty());
    }

    #[test]
    fn self_reference_is_dropped() {
        // An agent that body-modified their own `login` plus calls it elsewhere
        // shouldn't generate a self-impact.
        let sig = ContentHash::from_bytes(b"sig");
        let mut new_sym = sym("login", "crate::auth", SymbolKind::Function);
        new_sym.signature_hash = sig;
        let ops = vec![SemanticOperation::ModifySymbol {
            file: PathBuf::from("src/auth.rs"),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: sig,
            new_entry: new_sym.clone(),
        }];
        let refs = vec![sref(
            &new_sym.id.0,
            "login",
            Some("crate::auth"),
            ReferenceKind::Call,
            0..5,
        )];
        let footprint = make_footprint(refs, b"login()");
        let impacts = compute_impacts(&ops, &footprint);
        assert!(impacts.is_empty());
    }

    #[test]
    fn sort_order_severity_first_then_file() {
        // Two impacts: one deleted, one body-only. Deleted should come first.
        let ops = vec![
            SemanticOperation::DeleteSymbol {
                file: PathBuf::from("src/auth.rs"),
                id: SymbolId("crate::auth::gone::function".into()),
            },
            SemanticOperation::ModifySymbol {
                file: PathBuf::from("src/auth.rs"),
                old_hash: ContentHash::from_bytes(b"old"),
                old_signature_hash: ContentHash::from_bytes(b"sig"),
                new_entry: {
                    let mut s = sym("kept", "crate::auth", SymbolKind::Function);
                    s.signature_hash = ContentHash::from_bytes(b"sig");
                    s
                },
            },
        ];
        let refs = vec![
            sref(
                "crate::caller::function",
                "kept",
                None,
                ReferenceKind::Call,
                0..4,
            ),
            sref(
                "crate::caller::function",
                "gone",
                None,
                ReferenceKind::Call,
                10..14,
            ),
        ];
        let footprint = make_footprint(refs, b"kept()\n  gone()");
        let impacts = compute_impacts(&ops, &footprint);
        assert_eq!(impacts.len(), 2);
        assert_eq!(impacts[0].change, ImpactChange::Deleted);
        assert_eq!(impacts[1].change, ImpactChange::BodyOnlyChanged);
    }

    #[test]
    fn line_range_computes_1_based_lines() {
        let content = b"line1\nline2\nline3\n";
        assert_eq!(line_range(content, &(0..5)), (1, 1));
        assert_eq!(line_range(content, &(6..11)), (2, 2));
        // Range spanning a newline.
        assert_eq!(line_range(content, &(4..7)), (1, 2));
    }

    #[test]
    fn scope_matches_boundary_aware() {
        assert!(!scope_matches("crate::authenticator", "crate::auth"));
        assert!(scope_matches("crate::auth", "crate::auth"));
        assert!(scope_matches("my_crate::auth", "auth"));
    }
}
