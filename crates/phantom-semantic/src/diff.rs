//! Semantic diff: compares two symbol sets and produces [`SemanticOperation`]s.
//!
//! Uses Weave-style entity matching by composite key `(name, kind, scope)`.

use std::collections::HashMap;
use std::path::Path;

use phantom_core::changeset::SemanticOperation;
use phantom_core::symbol::{SymbolEntry, SymbolKind};

/// Entity identity key for Weave-style matching.
type EntityKey = (String, SymbolKind, String);

fn entity_key(entry: &SymbolEntry) -> EntityKey {
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
mod tests {
    use super::*;
    use phantom_core::id::{ContentHash, SymbolId};
    use std::path::PathBuf;

    fn make_symbol(name: &str, kind: SymbolKind, body: &str) -> SymbolEntry {
        let scope = "crate";
        let kind_str = format!("{kind:?}").to_lowercase();
        SymbolEntry {
            id: SymbolId(format!("{scope}::{name}::{kind_str}")),
            kind,
            name: name.to_string(),
            scope: scope.to_string(),
            file: PathBuf::from("test.rs"),
            byte_range: 0..body.len(),
            content_hash: ContentHash::from_bytes(body.as_bytes()),
        }
    }

    #[test]
    fn detects_add_and_modify() {
        let f1 = make_symbol("f1", SymbolKind::Function, "fn f1() { 1 }");
        let f2 = make_symbol("f2", SymbolKind::Function, "fn f2() {}");
        let f1_mod = make_symbol("f1", SymbolKind::Function, "fn f1() { 2 }");
        let f3 = make_symbol("f3", SymbolKind::Function, "fn f3() {}");

        let base = vec![f1, f2.clone()];
        let current = vec![f1_mod, f2, f3];

        let ops = diff_symbols(&base, &current, Path::new("test.rs"));

        let adds: Vec<_> = ops
            .iter()
            .filter(|o| matches!(o, SemanticOperation::AddSymbol { .. }))
            .collect();
        let mods: Vec<_> = ops
            .iter()
            .filter(|o| matches!(o, SemanticOperation::ModifySymbol { .. }))
            .collect();
        assert_eq!(adds.len(), 1); // f3
        assert_eq!(mods.len(), 1); // f1
    }

    #[test]
    fn detects_deletion() {
        let f1 = make_symbol("f1", SymbolKind::Function, "fn f1() {}");
        let f2 = make_symbol("f2", SymbolKind::Function, "fn f2() {}");

        let ops = diff_symbols(&[f1, f2], &[make_symbol("f1", SymbolKind::Function, "fn f1() {}")], Path::new("test.rs"));

        let deletes: Vec<_> = ops
            .iter()
            .filter(|o| matches!(o, SemanticOperation::DeleteSymbol { .. }))
            .collect();
        assert_eq!(deletes.len(), 1);
    }

    #[test]
    fn identical_files_produce_empty_diff() {
        let f1 = make_symbol("f1", SymbolKind::Function, "fn f1() {}");
        let ops = diff_symbols(&[f1.clone()], &[f1], Path::new("test.rs"));
        assert!(ops.is_empty());
    }

    #[test]
    fn complete_rewrite() {
        let old1 = make_symbol("old1", SymbolKind::Function, "fn old1() {}");
        let old2 = make_symbol("old2", SymbolKind::Function, "fn old2() {}");
        let new1 = make_symbol("new1", SymbolKind::Function, "fn new1() {}");
        let new2 = make_symbol("new2", SymbolKind::Function, "fn new2() {}");

        let ops = diff_symbols(&[old1, old2], &[new1, new2], Path::new("test.rs"));

        let adds: Vec<_> = ops.iter().filter(|o| matches!(o, SemanticOperation::AddSymbol { .. })).collect();
        let deletes: Vec<_> = ops.iter().filter(|o| matches!(o, SemanticOperation::DeleteSymbol { .. })).collect();
        assert_eq!(adds.len(), 2);
        assert_eq!(deletes.len(), 2);
    }
}
