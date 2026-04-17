//! Semantic conflict detection for three-way merges.

use std::collections::HashMap;
use std::path::Path;

use phantom_core::conflict::{
    ConflictDetail, ConflictKind, ConflictSpan, MergeResult, MergeStrategy,
};
use phantom_core::error::CoreError;
use phantom_core::id::ChangesetId;
use phantom_core::symbol::SymbolEntry;

use crate::diff::{EntityKey, entity_key};
use crate::parser::Parser;

use super::reconstruct::reconstruct_merged_file;
use super::text::text_merge;

/// Attempt a semantic three-way merge using symbol-level analysis.
///
/// Parses all three versions, detects conflicts at the symbol level, and
/// reconstructs the merged file from symbol regions. Falls back to text merge
/// if the reconstructed file has syntax errors.
///
/// Returns the merge outcome paired with the strategy used to produce it:
/// [`MergeStrategy::Semantic`] for the normal path, or
/// [`MergeStrategy::TextFallbackInvalidSyntax`] when the reconstructed file
/// fails to re-parse and the function falls back to `text_merge`.
pub(super) fn semantic_merge(
    parser: &Parser,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    path: &Path,
) -> Result<(MergeResult, MergeStrategy), CoreError> {
    let base_symbols = parser
        .parse_file(path, base)
        .map_err(|e| CoreError::Semantic(e.to_string()))?;
    let ours_symbols = parser
        .parse_file(path, ours)
        .map_err(|e| CoreError::Semantic(e.to_string()))?;
    let theirs_symbols = parser
        .parse_file(path, theirs)
        .map_err(|e| CoreError::Semantic(e.to_string()))?;

    let base_map: HashMap<EntityKey, &SymbolEntry> =
        base_symbols.iter().map(|e| (entity_key(e), e)).collect();
    let ours_map: HashMap<EntityKey, &SymbolEntry> =
        ours_symbols.iter().map(|e| (entity_key(e), e)).collect();
    let theirs_map: HashMap<EntityKey, &SymbolEntry> =
        theirs_symbols.iter().map(|e| (entity_key(e), e)).collect();

    let mut conflicts = Vec::new();

    // Both-modified / both-added conflicts
    for (key, ours_entry) in &ours_map {
        let Some(theirs_entry) = theirs_map.get(key) else {
            continue;
        };
        let base_entry = base_map.get(key).copied();

        let (ours_changed, theirs_changed) = match base_entry {
            Some(b) => (
                ours_entry.content_hash != b.content_hash,
                theirs_entry.content_hash != b.content_hash,
            ),
            // Not in base: both sides added.
            None => (true, true),
        };
        if !(ours_changed && theirs_changed) {
            continue;
        }
        if ours_entry.content_hash == theirs_entry.content_hash {
            // Same content on both sides — deduplicate, no conflict.
            continue;
        }

        conflicts.push(both_modified_conflict(
            path,
            ours_entry,
            theirs_entry,
            base_entry,
            ours,
            theirs,
            base,
        ));
    }

    // Modify-delete conflicts
    for (key, base_entry) in &base_map {
        let in_ours = ours_map.get(key).copied();
        let in_theirs = theirs_map.get(key).copied();

        match (in_ours, in_theirs) {
            (Some(o), None) if o.content_hash != base_entry.content_hash => {
                conflicts.push(modify_delete_conflict(
                    path,
                    base_entry,
                    o,
                    base,
                    ours,
                    OtherSide::Ours,
                ));
            }
            (None, Some(t)) if t.content_hash != base_entry.content_hash => {
                conflicts.push(modify_delete_conflict(
                    path,
                    base_entry,
                    t,
                    base,
                    theirs,
                    OtherSide::Theirs,
                ));
            }
            _ => {}
        }
    }

    if !conflicts.is_empty() {
        return Ok((MergeResult::Conflict(conflicts), MergeStrategy::Semantic));
    }

    // No conflicts — reconstruct the merged file
    let merged = reconstruct_merged_file(
        base,
        ours,
        theirs,
        &base_symbols,
        &ours_symbols,
        &theirs_symbols,
    );

    // Safety net: re-parse the merged output and fall back to text merge
    // if the byte-range splicing produced broken syntax.
    if parser.has_syntax_errors(path, &merged) {
        tracing::warn!(
            ?path,
            "semantic merge produced invalid syntax, falling back to text merge"
        );
        return Ok((
            text_merge(base, ours, theirs, path),
            MergeStrategy::TextFallbackInvalidSyntax,
        ));
    }

    Ok((MergeResult::Clean(merged), MergeStrategy::Semantic))
}

/// Which side of a merge carried a surviving modification against the other side's deletion.
#[derive(Clone, Copy)]
enum OtherSide {
    Ours,
    Theirs,
}

/// Placeholder changeset id used when detecting conflicts outside an event-sourced context.
fn placeholder_changeset() -> ChangesetId {
    ChangesetId("unknown".into())
}

fn span_of(source: &[u8], entry: &SymbolEntry) -> ConflictSpan {
    ConflictSpan::from_byte_range(source, entry.byte_range.clone())
}

/// Build a `BothModifiedSymbol` conflict detail.
///
/// `base_entry` is `None` when both sides added the symbol (symbol absent from base).
fn both_modified_conflict(
    path: &Path,
    ours: &SymbolEntry,
    theirs: &SymbolEntry,
    base: Option<&SymbolEntry>,
    ours_src: &[u8],
    theirs_src: &[u8],
    base_src: &[u8],
) -> ConflictDetail {
    let description = if base.is_some() {
        format!("both sides modified {}::{}", ours.scope, ours.name)
    } else {
        format!(
            "both sides added {}::{} with different content",
            ours.scope, ours.name
        )
    };
    ConflictDetail {
        kind: ConflictKind::BothModifiedSymbol,
        file: path.to_path_buf(),
        symbol_id: Some(ours.id.clone()),
        ours_changeset: placeholder_changeset(),
        theirs_changeset: placeholder_changeset(),
        description,
        ours_span: Some(span_of(ours_src, ours)),
        theirs_span: Some(span_of(theirs_src, theirs)),
        base_span: base.map(|b| span_of(base_src, b)),
    }
}

/// Build a `ModifyDeleteSymbol` conflict detail.
///
/// `other` is the side that kept and modified the symbol; the other side deleted it.
fn modify_delete_conflict(
    path: &Path,
    base: &SymbolEntry,
    other: &SymbolEntry,
    base_src: &[u8],
    other_src: &[u8],
    side: OtherSide,
) -> ConflictDetail {
    let (description, ours_span, theirs_span) = match side {
        OtherSide::Ours => (
            format!(
                "ours modified {}::{} but theirs deleted it",
                base.scope, base.name
            ),
            Some(span_of(other_src, other)),
            None,
        ),
        OtherSide::Theirs => (
            format!(
                "theirs modified {}::{} but ours deleted it",
                base.scope, base.name
            ),
            None,
            Some(span_of(other_src, other)),
        ),
    };
    ConflictDetail {
        kind: ConflictKind::ModifyDeleteSymbol,
        file: path.to_path_buf(),
        symbol_id: Some(base.id.clone()),
        ours_changeset: placeholder_changeset(),
        theirs_changeset: placeholder_changeset(),
        description,
        ours_span,
        theirs_span,
        base_span: Some(span_of(base_src, base)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    fn merge(base: &str, ours: &str, theirs: &str) -> MergeResult {
        let parser = Parser::new();
        let (result, _strategy) = semantic_merge(
            &parser,
            base.as_bytes(),
            ours.as_bytes(),
            theirs.as_bytes(),
            Path::new("test.rs"),
        )
        .expect("parse should succeed");
        result
    }

    fn merge_with_strategy(base: &str, ours: &str, theirs: &str) -> (MergeResult, MergeStrategy) {
        let parser = Parser::new();
        semantic_merge(
            &parser,
            base.as_bytes(),
            ours.as_bytes(),
            theirs.as_bytes(),
            Path::new("test.rs"),
        )
        .expect("parse should succeed")
    }

    #[test]
    fn clean_semantic_merge_tagged_as_semantic() {
        let base = "fn a() { 1 }\n";
        let edited = "fn a() { 2 }\n";
        let (_r, strategy) = merge_with_strategy(base, edited, edited);
        assert_eq!(strategy, MergeStrategy::Semantic);
    }

    #[test]
    fn both_sides_modified_identically_is_not_a_conflict() {
        let base = "fn a() { 1 }\n";
        let edited = "fn a() { 2 }\n";
        let result = merge(base, edited, edited);
        assert!(
            matches!(result, MergeResult::Clean(_)),
            "same edit on both sides must deduplicate"
        );
    }

    #[test]
    fn both_sides_added_different_content_conflicts() {
        let base = "fn a() {}\n";
        let ours = "fn a() {}\nfn b() { 1 }\n";
        let theirs = "fn a() {}\nfn b() { 2 }\n";
        let result = merge(base, ours, theirs);
        let MergeResult::Conflict(details) = result else {
            panic!("expected conflict, got {result:?}");
        };
        assert_eq!(details.len(), 1);
        assert!(matches!(details[0].kind, ConflictKind::BothModifiedSymbol));
        assert!(
            details[0].base_span.is_none(),
            "both-added has no base span"
        );
        assert!(details[0].description.contains("added"));
    }

    #[test]
    fn modify_in_ours_delete_in_theirs_conflicts() {
        let base = "fn a() {}\nfn b() { 1 }\n";
        let ours = "fn a() {}\nfn b() { 99 }\n";
        let theirs = "fn a() {}\n";
        let result = merge(base, ours, theirs);
        let MergeResult::Conflict(details) = result else {
            panic!("expected conflict, got {result:?}");
        };
        assert_eq!(details.len(), 1);
        assert!(matches!(details[0].kind, ConflictKind::ModifyDeleteSymbol));
        assert!(details[0].ours_span.is_some());
        assert!(details[0].theirs_span.is_none());
        assert!(details[0].description.contains("ours modified"));
    }

    #[test]
    fn modify_in_theirs_delete_in_ours_conflicts() {
        let base = "fn a() {}\nfn b() { 1 }\n";
        let ours = "fn a() {}\n";
        let theirs = "fn a() {}\nfn b() { 99 }\n";
        let result = merge(base, ours, theirs);
        let MergeResult::Conflict(details) = result else {
            panic!("expected conflict, got {result:?}");
        };
        assert_eq!(details.len(), 1);
        assert!(matches!(details[0].kind, ConflictKind::ModifyDeleteSymbol));
        assert!(details[0].ours_span.is_none());
        assert!(details[0].theirs_span.is_some());
        assert!(details[0].description.contains("theirs modified"));
    }
}
