//! JSON symbol extraction via `tree-sitter-json`.
//!
//! Top-level object keys become `Section` symbols, enabling semantic merge when
//! two agents modify different keys in the same JSON file.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, node_text, push_symbol};

/// Extracts symbols from JSON files.
pub struct JsonExtractor;

impl LanguageExtractor for JsonExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_json::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["json"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        // JSON root is typically an "object" or "array" node.
        extract_json_top_level(root, source, file_path, &mut symbols);
        symbols
    }
}

fn extract_json_top_level(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "pair" => {
                // Extract the key string as the symbol name.
                if let Some(key_node) = child.child_by_field_name("key") {
                    let raw = node_text(key_node, source);
                    // Strip surrounding quotes from the key.
                    let key = raw.trim().trim_matches('"');
                    if !key.is_empty() {
                        push_symbol(
                            symbols,
                            "root",
                            key,
                            SymbolKind::Section,
                            child,
                            source,
                            file_path,
                        );
                    }
                }
            }
            // Recurse into the root object.
            "object" => {
                extract_json_top_level(child, source, file_path, symbols);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod tests;
