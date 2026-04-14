use super::*;
use phantom_core::symbol::SymbolKind;

#[test]
fn parses_rust_file() {
    let parser = Parser::new();
    let src = b"fn hello() {}";
    let symbols = parser.parse_file(Path::new("test.rs"), src).unwrap();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].kind, SymbolKind::Function);
}

#[test]
fn parses_typescript_file() {
    let parser = Parser::new();
    let src = b"function greet(): void {}";
    let symbols = parser.parse_file(Path::new("test.ts"), src).unwrap();
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function));
}

#[test]
fn parses_python_file() {
    let parser = Parser::new();
    let src = b"def hello():\n    pass";
    let symbols = parser.parse_file(Path::new("test.py"), src).unwrap();
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function));
}

#[test]
fn unsupported_extension_errors() {
    let parser = Parser::new();
    let result = parser.parse_file(Path::new("test.txt"), b"hello");
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        SemanticError::UnsupportedLanguage { .. }
    ));
}

#[test]
fn supports_language_checks() {
    let parser = Parser::new();
    assert!(parser.supports_language(Path::new("foo.rs")));
    assert!(parser.supports_language(Path::new("bar.ts")));
    assert!(parser.supports_language(Path::new("baz.py")));
    assert!(parser.supports_language(Path::new("qux.go")));
    assert!(parser.supports_language(Path::new("comp.tsx")));
    assert!(!parser.supports_language(Path::new("readme.md")));
    assert!(!parser.supports_language(Path::new("noext")));
}
