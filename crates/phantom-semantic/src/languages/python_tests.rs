use super::*;

fn parse_py(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = PythonExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.py"))
}

#[test]
fn extracts_functions_and_classes() {
    let src = r#"
def greet(name):
    return f"Hello, {name}"

class User:
    def __init__(self, name):
        self.name = name

    def get_name(self):
        return self.name
"#;
    let symbols = parse_py(src);
    assert!(
        symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Function && s.name == "greet")
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Class && s.name == "User")
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Method && s.name == "__init__")
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Method && s.name == "get_name")
    );
}

#[test]
fn extracts_imports() {
    let src = r#"
import os
from pathlib import Path
"#;
    let symbols = parse_py(src);
    let imports: Vec<_> = symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Import)
        .collect();
    assert_eq!(imports.len(), 2);
}
