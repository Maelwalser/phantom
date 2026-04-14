use super::*;

fn parse_json(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = JsonExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("package.json"))
}

#[test]
fn extracts_top_level_keys() {
    let src = r#"{
  "name": "my-app",
  "version": "1.0.0",
  "scripts": {
    "build": "tsc",
    "test": "jest"
  },
  "dependencies": {
    "react": "^18.0.0"
  }
}"#;
    let symbols = parse_json(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "name"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "version"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "scripts"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "dependencies"));
}

#[test]
fn handles_empty_object() {
    let src = "{}";
    let symbols = parse_json(src);
    assert!(symbols.is_empty());
}

#[test]
fn tsconfig_keys() {
    let src = r#"{
  "compilerOptions": {
    "target": "es2020"
  },
  "include": ["src"]
}"#;
    let symbols = parse_json(src);
    assert!(symbols.iter().any(|s| s.name == "compilerOptions"));
    assert!(symbols.iter().any(|s| s.name == "include"));
}
