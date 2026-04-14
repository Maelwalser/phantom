use super::*;

fn parse_toml(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = TomlExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("Cargo.toml"))
}

#[test]
fn extracts_tables_and_root_pairs() {
    let src = r#"
[package]
name = "phantom"
version = "0.1.0"

[dependencies]
serde = "1"

[dev-dependencies]
tempfile = "3"
"#;
    let symbols = parse_toml(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name.contains("package")));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name.contains("dependencies")));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name.contains("dev-dependencies")));
}

#[test]
fn extracts_dotted_tables() {
    let src = r#"
[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
"#;
    let symbols = parse_toml(src);
    assert!(!symbols.is_empty());
    // The table should be extracted as a section.
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section));
}

#[test]
fn extracts_bare_root_keys() {
    let src = r#"
name = "test"
version = "1.0"
"#;
    let symbols = parse_toml(src);
    assert!(symbols.iter().any(|s| s.name == "name"));
    assert!(symbols.iter().any(|s| s.name == "version"));
}
