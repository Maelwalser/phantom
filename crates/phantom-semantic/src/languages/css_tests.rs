use super::*;

fn parse_css(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = CssExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("styles.css"))
}

#[test]
fn extracts_rule_sets() {
    let src = r#"
body {
    margin: 0;
    padding: 0;
}

.container {
    max-width: 1200px;
}

#header {
    background: blue;
}
"#;
    let symbols = parse_css(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "body"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == ".container"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "#header"));
}

#[test]
fn extracts_media_queries() {
    let src = r#"
@media (min-width: 768px) {
    .container { max-width: 720px; }
}
"#;
    let symbols = parse_css(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name.contains("@media")));
}

#[test]
fn extracts_import_directives() {
    let src = r#"
@import url("reset.css");
body { color: black; }
"#;
    let symbols = parse_css(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Directive));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "body"));
}
