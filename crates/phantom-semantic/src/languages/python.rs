//! Python symbol extraction via `tree-sitter-python`.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{
    LanguageExtractor, build_scope, child_field_text, for_each_named_child, node_text,
    push_symbol,
};

const ROOT_SCOPE: &str = "module";

/// Extracts symbols from Python source files.
pub struct PythonExtractor;

impl LanguageExtractor for PythonExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["py"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_py_node(root, source, file_path, &[], &mut symbols);
        symbols
    }
}

fn extract_py_node(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    scope_parts: &[String],
    symbols: &mut Vec<SymbolEntry>,
) {
    let kind = node.kind();
    match kind {
        "function_definition" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, ROOT_SCOPE);
                let sym_kind = if scope_parts.is_empty() {
                    SymbolKind::Function
                } else {
                    SymbolKind::Method
                };
                push_symbol(symbols, &scope, &name, sym_kind, node, source, file_path);
            }
        }
        "class_definition" => {
            let Some(name) = child_field_text(node, "name", source) else {
                return;
            };
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            push_symbol(symbols, &scope, &name, SymbolKind::Class, node, source, file_path);
            if let Some(body) = node.child_by_field_name("body") {
                let mut new_scope = scope_parts.to_vec();
                new_scope.push(name);
                for_each_named_child(body, |child| {
                    extract_py_node(child, source, file_path, &new_scope, symbols);
                });
            }
            return;
        }
        "import_statement" | "import_from_statement" => {
            let text = node_text(node, source);
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            push_symbol(symbols, &scope, &text, SymbolKind::Import, node, source, file_path);
        }
        _ => {}
    }

    // Recurse into top-level containers
    if matches!(kind, "module" | "block") {
        for_each_named_child(node, |child| {
            extract_py_node(child, source, file_path, scope_parts, symbols);
        });
    }
}

#[cfg(test)]
mod tests {
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
}
