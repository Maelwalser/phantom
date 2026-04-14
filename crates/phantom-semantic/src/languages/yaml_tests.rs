use super::*;

fn parse_yaml(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = YamlExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("config.yml"))
}

#[test]
fn extracts_top_level_mapping_keys() {
    let src = r#"
name: my-project
version: "1.0"
dependencies:
  foo: "1.2"
  bar: "3.4"
scripts:
  build: cargo build
  test: cargo test
"#;
    let symbols = parse_yaml(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "name"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "version"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "dependencies"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "scripts"));
}

#[test]
fn extracts_github_actions_workflow() {
    let src = r#"
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
  test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo test
"#;
    let symbols = parse_yaml(src);
    assert!(symbols.iter().any(|s| s.name == "name"));
    assert!(symbols.iter().any(|s| s.name == "on"));
    assert!(symbols.iter().any(|s| s.name == "jobs"));
}

#[test]
fn all_symbols_have_section_kind() {
    let src = "key1: value1\nkey2: value2\n";
    let symbols = parse_yaml(src);
    assert!(!symbols.is_empty());
    for s in &symbols {
        assert_eq!(s.kind, SymbolKind::Section);
    }
}
