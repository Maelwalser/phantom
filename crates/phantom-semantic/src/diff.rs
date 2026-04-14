//! Semantic diff: compares two symbol sets and produces [`SemanticOperation`]s.
//!
//! Uses Weave-style entity matching by composite key `(name, kind, scope)`.

use std::collections::HashMap;
use std::path::Path;

use phantom_core::changeset::SemanticOperation;
use phantom_core::symbol::{SymbolEntry, SymbolKind};

/// Entity identity key for Weave-style matching.
pub type EntityKey = (String, SymbolKind, String);

/// Extract the composite identity key from a symbol entry.
pub fn entity_key(entry: &SymbolEntry) -> EntityKey {
    (entry.name.clone(), entry.kind, entry.scope.clone())
}

/// Compute the semantic operations needed to transform `base` into `current`.
///
/// Matches symbols by their composite identity `(name, kind, scope)` and
/// detects adds, modifications, and deletions.
pub fn diff_symbols(
    base: &[SymbolEntry],
    current: &[SymbolEntry],
    file: &Path,
) -> Vec<SemanticOperation> {
    let base_map: HashMap<EntityKey, &SymbolEntry> =
        base.iter().map(|e| (entity_key(e), e)).collect();
    let current_map: HashMap<EntityKey, &SymbolEntry> =
        current.iter().map(|e| (entity_key(e), e)).collect();

    let mut ops = Vec::new();

    // Additions and modifications
    for (key, new_entry) in &current_map {
        match base_map.get(key) {
            None => {
                ops.push(SemanticOperation::AddSymbol {
                    file: file.to_path_buf(),
                    symbol: (*new_entry).clone(),
                });
            }
            Some(old_entry) => {
                if old_entry.content_hash != new_entry.content_hash {
                    ops.push(SemanticOperation::ModifySymbol {
                        file: file.to_path_buf(),
                        old_hash: old_entry.content_hash,
                        new_entry: (*new_entry).clone(),
                    });
                }
            }
        }
    }

    // Deletions
    for (key, old_entry) in &base_map {
        if !current_map.contains_key(key) {
            ops.push(SemanticOperation::DeleteSymbol {
                file: file.to_path_buf(),
                id: old_entry.id.clone(),
            });
        }
    }

    // Sort by operation type for deterministic output (adds, then modifies, then deletes)
    ops.sort_by_key(|op| match op {
        SemanticOperation::AddSymbol { .. } => 0,
        SemanticOperation::ModifySymbol { .. } => 1,
        SemanticOperation::DeleteSymbol { .. } => 2,
        _ => 3,
    });

    ops
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
