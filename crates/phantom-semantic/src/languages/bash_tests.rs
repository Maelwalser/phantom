use super::*;

fn parse_bash(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = BashExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("script.sh"))
}

#[test]
fn extracts_functions() {
    let src = r#"#!/bin/bash

build() {
    cargo build --release
}

function test_all {
    cargo test
}
"#;
    let symbols = parse_bash(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function && s.name == "build"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function && s.name == "test_all"));
}

#[test]
fn extracts_variable_assignments() {
    let src = r#"
VERSION="1.0.0"
BUILD_DIR=/tmp/build
"#;
    let symbols = parse_bash(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Variable && s.name == "VERSION"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Variable && s.name == "BUILD_DIR"));
}

#[test]
fn mixed_functions_and_variables() {
    let src = r#"
APP_NAME="myapp"

setup() {
    mkdir -p /opt/$APP_NAME
}
"#;
    let symbols = parse_bash(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Variable));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function));
}
