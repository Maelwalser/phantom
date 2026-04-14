use super::*;

fn parse_ts(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = TypeScriptExtractor::new();
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.ts"))
}

#[test]
fn extracts_function_and_class() {
    let src = r#"
function greet(name: string): string {
    return `Hello, ${name}`;
}

class User {
    getName(): string {
        return this.name;
    }
}
"#;
    let symbols = parse_ts(src);
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
            .any(|s| s.kind == SymbolKind::Method && s.name == "getName")
    );
}

#[test]
fn extracts_imports_and_interfaces() {
    let src = r#"
import { useState } from 'react';

interface Props {
    name: string;
}

type Result<T> = { ok: true; value: T } | { ok: false; error: Error };
"#;
    let symbols = parse_ts(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    assert!(
        symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Interface && s.name == "Props")
    );
    assert!(
        symbols
            .iter()
            .any(|s| s.kind == SymbolKind::TypeAlias && s.name == "Result")
    );
}
