use super::*;

fn sample_entry() -> SymbolEntry {
    SymbolEntry {
        id: SymbolId("crate::handlers::login::Function".into()),
        kind: SymbolKind::Function,
        name: "login".into(),
        scope: "crate::handlers".into(),
        file: PathBuf::from("src/handlers.rs"),
        byte_range: 100..250,
        content_hash: ContentHash::from_bytes(b"fn login() {}"),
    }
}

#[test]
fn serde_symbol_kind_roundtrip() {
    let kind = SymbolKind::Trait;
    let json = serde_json::to_string(&kind).unwrap();
    let back: SymbolKind = serde_json::from_str(&json).unwrap();
    assert_eq!(kind, back);
}

#[test]
fn serde_symbol_entry_roundtrip() {
    let entry = sample_entry();
    let json = serde_json::to_string(&entry).unwrap();
    let back: SymbolEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(entry, back);
}

fn make_symbol(name: &str, kind: SymbolKind, byte_range: Range<usize>) -> SymbolEntry {
    SymbolEntry {
        id: SymbolId(format!("test::{name}::{kind:?}")),
        kind,
        name: name.into(),
        scope: "test".into(),
        file: PathBuf::from("test.rs"),
        byte_range,
        content_hash: ContentHash::from_bytes(name.as_bytes()),
    }
}

#[test]
fn find_enclosing_symbol_returns_tightest() {
    let symbols = vec![
        make_symbol("MyImpl", SymbolKind::Impl, 0..500),
        make_symbol("inner_method", SymbolKind::Method, 50..200),
    ];
    let target = 100..150;
    let result = find_enclosing_symbol(&symbols, &target);
    assert_eq!(result.unwrap().name, "inner_method");
}

#[test]
fn find_enclosing_symbol_exact_match() {
    let symbols = vec![make_symbol("foo", SymbolKind::Function, 10..50)];
    let target = 10..50;
    let result = find_enclosing_symbol(&symbols, &target);
    assert_eq!(result.unwrap().name, "foo");
}

#[test]
fn find_enclosing_symbol_none_when_no_match() {
    let symbols = vec![make_symbol("foo", SymbolKind::Function, 10..50)];
    let target = 60..80;
    assert!(find_enclosing_symbol(&symbols, &target).is_none());
}
