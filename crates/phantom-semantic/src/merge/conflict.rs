//! Semantic conflict detection for three-way merges.

use std::collections::HashMap;
use std::path::Path;

use phantom_core::conflict::{ConflictDetail, ConflictKind, ConflictSpan};
use phantom_core::error::CoreError;
use phantom_core::id::ChangesetId;
use phantom_core::symbol::SymbolEntry;
use phantom_core::traits::MergeResult;

use crate::diff::{EntityKey, entity_key};
use crate::parser::Parser;

use super::reconstruct::reconstruct_merged_file;
use super::text::text_merge;

/// Attempt a semantic three-way merge using symbol-level analysis.
///
/// Parses all three versions, detects conflicts at the symbol level, and
/// reconstructs the merged file from symbol regions. Falls back to text merge
/// if the reconstructed file has syntax errors.
pub(super) fn semantic_merge(
    parser: &Parser,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    path: &Path,
) -> Result<MergeResult, CoreError> {
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
    let placeholder_cs = ChangesetId("unknown".into());

    // Check for conflicts
    for (key, ours_entry) in &ours_map {
        if let Some(theirs_entry) = theirs_map.get(key) {
            if let Some(base_entry) = base_map.get(key) {
                let ours_changed = ours_entry.content_hash != base_entry.content_hash;
                let theirs_changed = theirs_entry.content_hash != base_entry.content_hash;
                if ours_changed && theirs_changed {
                    // Both modified same symbol
                    if ours_entry.content_hash != theirs_entry.content_hash {
                        conflicts.push(ConflictDetail {
                            kind: ConflictKind::BothModifiedSymbol,
                            file: path.to_path_buf(),
                            symbol_id: Some(ours_entry.id.clone()),
                            ours_changeset: placeholder_cs.clone(),
                            theirs_changeset: placeholder_cs.clone(),
                            description: format!(
                                "both sides modified {}::{}",
                                ours_entry.scope, ours_entry.name
                            ),
                            ours_span: Some(ConflictSpan::from_byte_range(
                                ours,
                                ours_entry.byte_range.clone(),
                            )),
                            theirs_span: Some(ConflictSpan::from_byte_range(
                                theirs,
                                theirs_entry.byte_range.clone(),
                            )),
                            base_span: Some(ConflictSpan::from_byte_range(
                                base,
                                base_entry.byte_range.clone(),
                            )),
                        });
                    }
                    // If both changed to same content, no conflict (deduplicate)
                }
            } else {
                // Both added same-named symbol (not in base)
                if ours_entry.content_hash != theirs_entry.content_hash {
                    conflicts.push(ConflictDetail {
                        kind: ConflictKind::BothModifiedSymbol,
                        file: path.to_path_buf(),
                        symbol_id: Some(ours_entry.id.clone()),
                        ours_changeset: placeholder_cs.clone(),
                        theirs_changeset: placeholder_cs.clone(),
                        description: format!(
                            "both sides added {}::{} with different content",
                            ours_entry.scope, ours_entry.name
                        ),
                        ours_span: Some(ConflictSpan::from_byte_range(
                            ours,
                            ours_entry.byte_range.clone(),
                        )),
                        theirs_span: Some(ConflictSpan::from_byte_range(
                            theirs,
                            theirs_entry.byte_range.clone(),
                        )),
                        base_span: None,
                    });
                }
                // Same content → deduplicate, no conflict
            }
        }
    }

    // Modify-delete conflicts
    for (key, base_entry) in &base_map {
        let in_ours = ours_map.contains_key(key);
        let in_theirs = theirs_map.contains_key(key);

        if in_ours && !in_theirs {
            let ours_entry = ours_map[key];
            if ours_entry.content_hash != base_entry.content_hash {
                conflicts.push(ConflictDetail {
                    kind: ConflictKind::ModifyDeleteSymbol,
                    file: path.to_path_buf(),
                    symbol_id: Some(base_entry.id.clone()),
                    ours_changeset: placeholder_cs.clone(),
                    theirs_changeset: placeholder_cs.clone(),
                    description: format!(
                        "ours modified {}::{} but theirs deleted it",
                        base_entry.scope, base_entry.name
                    ),
                    ours_span: Some(ConflictSpan::from_byte_range(
                        ours,
                        ours_entry.byte_range.clone(),
                    )),
                    theirs_span: None,
                    base_span: Some(ConflictSpan::from_byte_range(
                        base,
                        base_entry.byte_range.clone(),
                    )),
                });
            }
        } else if !in_ours && in_theirs {
            let theirs_entry = theirs_map[key];
            if theirs_entry.content_hash != base_entry.content_hash {
                conflicts.push(ConflictDetail {
                    kind: ConflictKind::ModifyDeleteSymbol,
                    file: path.to_path_buf(),
                    symbol_id: Some(base_entry.id.clone()),
                    ours_changeset: placeholder_cs.clone(),
                    theirs_changeset: placeholder_cs.clone(),
                    description: format!(
                        "theirs modified {}::{} but ours deleted it",
                        base_entry.scope, base_entry.name
                    ),
                    ours_span: None,
                    theirs_span: Some(ConflictSpan::from_byte_range(
                        theirs,
                        theirs_entry.byte_range.clone(),
                    )),
                    base_span: Some(ConflictSpan::from_byte_range(
                        base,
                        base_entry.byte_range.clone(),
                    )),
                });
            }
        }
    }

    if !conflicts.is_empty() {
        return Ok(MergeResult::Conflict(conflicts));
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
        return text_merge(base, ours, theirs, path);
    }

    Ok(MergeResult::Clean(merged))
}
