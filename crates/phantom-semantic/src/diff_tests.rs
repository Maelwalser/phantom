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

    let ops = diff_symbols(
        &[f1, f2],
        &[make_symbol("f1", SymbolKind::Function, "fn f1() {}")],
        Path::new("test.rs"),
    );

    let deletes: Vec<_> = ops
        .iter()
        .filter(|o| matches!(o, SemanticOperation::DeleteSymbol { .. }))
        .collect();
    assert_eq!(deletes.len(), 1);
}

#[test]
fn identical_files_produce_empty_diff() {
    let f1 = make_symbol("f1", SymbolKind::Function, "fn f1() {}");
    let ops = diff_symbols(
        std::slice::from_ref(&f1),
        std::slice::from_ref(&f1),
        Path::new("test.rs"),
    );
    assert!(ops.is_empty());
}

#[test]
fn complete_rewrite() {
    let old1 = make_symbol("old1", SymbolKind::Function, "fn old1() {}");
    let old2 = make_symbol("old2", SymbolKind::Function, "fn old2() {}");
    let new1 = make_symbol("new1", SymbolKind::Function, "fn new1() {}");
    let new2 = make_symbol("new2", SymbolKind::Function, "fn new2() {}");

    let ops = diff_symbols(&[old1, old2], &[new1, new2], Path::new("test.rs"));

    let adds: Vec<_> = ops
        .iter()
        .filter(|o| matches!(o, SemanticOperation::AddSymbol { .. }))
        .collect();
    let deletes: Vec<_> = ops
        .iter()
        .filter(|o| matches!(o, SemanticOperation::DeleteSymbol { .. }))
        .collect();
    assert_eq!(adds.len(), 2);
    assert_eq!(deletes.len(), 2);
}
