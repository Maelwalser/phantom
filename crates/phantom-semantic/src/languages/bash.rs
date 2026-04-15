//! Bash/Shell symbol extraction via `tree-sitter-bash`.
//!
//! Functions become `Function` symbols and top-level variable assignments become
//! `Variable` symbols, enabling semantic merge of shell scripts.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, child_field_text, node_text, push_symbol};

/// Extracts symbols from shell scripts.
pub struct BashExtractor;

impl LanguageExtractor for BashExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_bash::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["sh", "bash", "zsh"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_bash_top_level(root, source, file_path, &mut symbols);
        symbols
    }
}

fn extract_bash_top_level(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                let name = child_field_text(child, "name", source)
                    .unwrap_or_else(|| extract_function_name(child, source));
                if !name.is_empty() {
                    push_symbol(
                        symbols,
                        "script",
                        &name,
                        SymbolKind::Function,
                        child,
                        source,
                        file_path,
                    );
                }
            }
            "variable_assignment" => {
                let name = child_field_text(child, "name", source)
                    .unwrap_or_else(|| extract_var_name(child, source));
                if !name.is_empty() {
                    push_symbol(
                        symbols,
                        "script",
                        &name,
                        SymbolKind::Variable,
                        child,
                        source,
                        file_path,
                    );
                }
            }
            "declaration_command" => {
                // Handles `export VAR=value`, `local VAR=value`, etc.
                let text = node_text(child, source);
                let name = text
                    .split_whitespace()
                    .find(|s| s.contains('='))
                    .map(|s| s.split('=').next().unwrap_or(""))
                    .or_else(|| text.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    push_symbol(
                        symbols,
                        "script",
                        &name,
                        SymbolKind::Variable,
                        child,
                        source,
                        file_path,
                    );
                }
            }
            // Recurse into top-level program node.
            "program" => {
                extract_bash_top_level(child, source, file_path, symbols);
            }
            _ => {}
        }
    }
}

/// Extract function name from the first child word node.
fn extract_function_name(node: Node<'_>, source: &[u8]) -> String {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "word" {
            return node_text(child, source).trim().to_string();
        }
    }
    String::new()
}

/// Extract variable name from a variable_assignment node.
fn extract_var_name(node: Node<'_>, source: &[u8]) -> String {
    let text = node_text(node, source);
    text.split('=').next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
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
}
