//! Python symbol extraction via `tree-sitter-python`.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, build_scope, child_field_text, node_text, push_symbol};

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
                let scope = build_scope(scope_parts, "module");
                let sym_kind = if !scope_parts.is_empty() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                push_symbol(symbols, &scope, &name, sym_kind, node, source, file_path);
            }
        }
        "class_definition" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "module");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Class,
                    node,
                    source,
                    file_path,
                );
                // Recurse into class body
                if let Some(body) = node.child_by_field_name("body") {
                    let mut new_scope = scope_parts.to_vec();
                    new_scope.push(name);
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        extract_py_node(child, source, file_path, &new_scope, symbols);
                    }
                }
                return;
            }
        }
        "import_statement" | "import_from_statement" => {
            let text = node_text(node, source);
            let scope = build_scope(scope_parts, "module");
            push_symbol(
                symbols,
                &scope,
                &text,
                SymbolKind::Import,
                node,
                source,
                file_path,
            );
        }
        _ => {}
    }

    // Recurse into top-level containers
    if matches!(kind, "module" | "block") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            extract_py_node(child, source, file_path, scope_parts, symbols);
        }
    }
}

#[cfg(test)]
#[path = "python_tests.rs"]
mod tests;
