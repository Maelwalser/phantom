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
use phantom_core::symbol::{ReferenceKind, SymbolEntry, SymbolReference};
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
                if !reference_kind_affected_by(reference.kind, changed.change) {
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

/// Decide whether a reference of the given kind is semantically affected by
/// a trunk change of the given kind.
///
/// Imports care about deletions (the import line fails to resolve) but not
/// about signature or body changes to the exported symbol — those only
/// matter to call sites. This filter keeps the notification focused on the
/// places the agent actually has to review.
fn reference_kind_affected_by(reference: ReferenceKind, change: ImpactChange) -> bool {
    match (reference, change) {
        // Imports care about existence, not shape.
        (ReferenceKind::Import, ImpactChange::Deleted) => true,
        (ReferenceKind::Import, _) => false,
        // All other reference kinds (Call, TypeUse, FieldAccess, TraitImpl)
        // are affected by every change class.
        _ => true,
    }
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
/// Returns a safe fallback label "{kind} `{name}`" that is always accurate.
/// Enriched previews (actual declaration snippets) are attached later via
/// [`enrich_trunk_previews`] when the caller can supply trunk-side file
/// contents.
fn preview_snippet(entry: &SymbolEntry) -> String {
    format!("{} `{}`", entry.kind, entry.name)
}

/// Trunk-side file content lookup used to render richer `trunk_preview`
/// snippets for impacts.
///
/// Keyed by the file path from the semantic operation. The value is the
/// full byte content of that file at the new trunk HEAD.
pub type TrunkContentMap = HashMap<PathBuf, Vec<u8>>;

/// Enrich each impact's `trunk_preview` with the actual declaration text
/// from the new trunk version of the symbol.
///
/// For [`ImpactChange::SignatureChanged`], the preview is rendered as
/// `"signature: {old_preview} → {new_preview}"` when the caller also
/// supplies `base_contents` for the pre-materialization file state. For
/// other impact kinds only the new-side snippet is used. Falls back to the
/// existing "{kind} `{name}`" label when content is missing.
///
/// Mutates `impacts` in place.
pub fn enrich_trunk_previews(
    impacts: &mut [DependencyImpact],
    operations: &[SemanticOperation],
    new_contents: &TrunkContentMap,
    base_contents: &TrunkContentMap,
) {
    let ops_by_id = operations
        .iter()
        .filter_map(|op| op.symbol_id().map(|id| (id.clone(), op)))
        .collect::<HashMap<_, _>>();

    for impact in impacts.iter_mut() {
        let Some(op) = ops_by_id.get(&impact.depends_on) else {
            continue;
        };
        let new_preview = new_symbol_preview(op, new_contents);
        let rendered = match impact.change {
            ImpactChange::SignatureChanged => {
                let old = old_symbol_preview(op, base_contents);
                match (old, new_preview) {
                    (Some(o), Some(n)) if o != n => Some(format!("signature: {o}  →  {n}")),
                    (_, Some(n)) => Some(format!("new signature: {n}")),
                    (_, None) => None,
                }
            }
            ImpactChange::Deleted => Some("symbol deleted from trunk".into()),
            _ => new_preview.map(|n| format!("new: {n}")),
        };
        if let Some(r) = rendered {
            impact.trunk_preview = Some(r);
        }
    }
}

/// Render the new-side declaration snippet for an operation.
fn new_symbol_preview(op: &SemanticOperation, contents: &TrunkContentMap) -> Option<String> {
    let entry = match op {
        SemanticOperation::AddSymbol { symbol, .. } => symbol,
        SemanticOperation::ModifySymbol { new_entry, .. } => new_entry,
        _ => return None,
    };
    let content = contents.get(&entry.file)?;
    snippet_from_range(
        content,
        &entry.byte_range,
        /*prefer_signature=*/ true,
        120,
    )
}

/// Render the old-side declaration snippet for a `ModifySymbol`.
///
/// Uses the pre-materialization file content (if supplied) and applies a
/// name-based search in lieu of the old byte_range, which we don't carry in
/// the event. Returns `None` when the symbol can't be located.
fn old_symbol_preview(op: &SemanticOperation, base_contents: &TrunkContentMap) -> Option<String> {
    let SemanticOperation::ModifySymbol {
        file, new_entry, ..
    } = op
    else {
        return None;
    };
    let content = base_contents.get(file)?;
    // Best-effort: find the first line containing the symbol name — a real
    // pre-image parse would be more accurate but costs a tree-sitter pass
    // per impact. The line-match heuristic is acceptable because the
    // snippet is purely informational and dependents see both the old and
    // new line in the notification.
    let name_bytes = new_entry.name.as_bytes();
    let pos = find_subslice(content, name_bytes)?;
    // Expand to the line containing `pos`.
    let line_start = content[..pos]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let line_end = content[pos..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(content.len(), |i| pos + i);
    let bytes = &content[line_start..line_end.min(line_start + 120)];
    Some(String::from_utf8_lossy(bytes).trim().to_string())
}

/// Extract a short single-line preview from a byte slice at `range`.
///
/// When `prefer_signature` is true, truncates at the first `{` or newline
/// to avoid pulling in the full function body. Always bounded to
/// `max_bytes` characters.
fn snippet_from_range(
    content: &[u8],
    range: &std::ops::Range<usize>,
    prefer_signature: bool,
    max_bytes: usize,
) -> Option<String> {
    if range.start >= content.len() {
        return None;
    }
    let end = range.end.min(content.len());
    let slice = &content[range.start..end];
    let cut = if prefer_signature {
        slice
            .iter()
            .position(|&b| b == b'{' || b == b'\n')
            .unwrap_or(slice.len())
    } else {
        slice.len()
    };
    let cut = cut.min(max_bytes);
    let text = String::from_utf8_lossy(&slice[..cut]).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
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

    #[test]
    fn enrich_trunk_previews_renders_signature_diff() {
        let file = PathBuf::from("src/auth.rs");
        let old_content = b"fn login(user_id: u32) -> bool { true }\n";
        let new_content = b"fn login(user_id: u32, token: &str) -> bool { true }\n";
        let mut new_sym = sym("login", "crate::auth", SymbolKind::Function);
        new_sym.file = file.clone();
        new_sym.byte_range = 0..53; // covers "fn login(user_id: u32, token: &str) -> bool {"
        new_sym.signature_hash = ContentHash::from_bytes(b"new_sig");

        let ops = vec![SemanticOperation::ModifySymbol {
            file: file.clone(),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: ContentHash::from_bytes(b"old_sig"),
            new_entry: new_sym.clone(),
        }];
        let mut impacts = vec![DependencyImpact {
            your_symbol: SymbolId("crate::caller::function".into()),
            depends_on: new_sym.id.clone(),
            change: ImpactChange::SignatureChanged,
            edge_kind: ReferenceKind::Call,
            file: PathBuf::from("src/caller.rs"),
            byte_range: 0..5,
            line_range: (1, 1),
            trunk_preview: None,
        }];
        let mut new_contents = TrunkContentMap::new();
        new_contents.insert(file.clone(), new_content.to_vec());
        let mut base_contents = TrunkContentMap::new();
        base_contents.insert(file, old_content.to_vec());

        enrich_trunk_previews(&mut impacts, &ops, &new_contents, &base_contents);
        let preview = impacts[0].trunk_preview.as_ref().expect("expected preview");
        assert!(
            preview.contains("signature:"),
            "expected signature diff marker, got {preview}"
        );
        assert!(
            preview.contains("token"),
            "expected new param in preview, got {preview}"
        );
    }

    #[test]
    fn enrich_trunk_previews_marks_deletions() {
        let file = PathBuf::from("src/auth.rs");
        let ops = vec![SemanticOperation::DeleteSymbol {
            file: file.clone(),
            id: SymbolId("crate::auth::login::function".into()),
        }];
        let mut impacts = vec![DependencyImpact {
            your_symbol: SymbolId("crate::caller::function".into()),
            depends_on: SymbolId("crate::auth::login::function".into()),
            change: ImpactChange::Deleted,
            edge_kind: ReferenceKind::Call,
            file: PathBuf::from("src/caller.rs"),
            byte_range: 0..5,
            line_range: (1, 1),
            trunk_preview: None,
        }];
        enrich_trunk_previews(
            &mut impacts,
            &ops,
            &TrunkContentMap::new(),
            &TrunkContentMap::new(),
        );
        let preview = impacts[0].trunk_preview.as_ref().expect("preview present");
        assert!(preview.contains("deleted"));
    }

    #[test]
    fn import_refs_filtered_for_non_delete_changes() {
        // A signature change should NOT generate an impact for the import
        // statement that references the same name — only for call sites.
        let new_sym = {
            let mut s = sym("login", "crate::auth", SymbolKind::Function);
            s.signature_hash = ContentHash::from_bytes(b"new_sig");
            s
        };
        let ops = vec![SemanticOperation::ModifySymbol {
            file: PathBuf::from("src/auth.rs"),
            old_hash: ContentHash::from_bytes(b"old"),
            old_signature_hash: ContentHash::from_bytes(b"old_sig"),
            new_entry: new_sym,
        }];
        let refs = vec![
            sref(
                "module::import_stmt::import",
                "login",
                Some("crate::auth"),
                ReferenceKind::Import,
                0..5,
            ),
            sref(
                "module::caller::function",
                "login",
                Some("crate::auth"),
                ReferenceKind::Call,
                50..55,
            ),
        ];
        let footprint = make_footprint(refs, b"dummy content");
        let impacts = compute_impacts(&ops, &footprint);
        assert_eq!(impacts.len(), 1);
        assert_eq!(impacts[0].edge_kind, ReferenceKind::Call);
    }

    #[test]
    fn import_refs_still_emit_for_deletions() {
        let ops = vec![SemanticOperation::DeleteSymbol {
            file: PathBuf::from("src/auth.rs"),
            id: SymbolId("crate::auth::login::function".into()),
        }];
        let refs = vec![sref(
            "module::import_stmt::import",
            "login",
            Some("crate::auth"),
            ReferenceKind::Import,
            0..5,
        )];
        let footprint = make_footprint(refs, b"dummy");
        let impacts = compute_impacts(&ops, &footprint);
        assert_eq!(impacts.len(), 1);
        assert_eq!(impacts[0].edge_kind, ReferenceKind::Import);
        assert_eq!(impacts[0].change, ImpactChange::Deleted);
    }

    #[test]
    fn snippet_stops_at_opening_brace() {
        let content = b"fn foo(x: u32) -> bool {\n    body\n}";
        let s = snippet_from_range(content, &(0..content.len()), true, 200).unwrap();
        assert_eq!(s, "fn foo(x: u32) -> bool");
    }
}
