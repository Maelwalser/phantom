//! YAML symbol extraction via `tree-sitter-yaml`.
//!
//! Top-level mapping keys become `Section` symbols, enabling semantic merge
//! of independent YAML sections (e.g., different CI jobs, docker-compose services).

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, node_text, push_symbol};

/// Extracts symbols from YAML files.
pub struct YamlExtractor;

impl LanguageExtractor for YamlExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_yaml::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["yml", "yaml"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_yaml_recursive(root, source, file_path, &mut symbols);
        symbols
    }
}

/// Recursively walk the tree looking for `block_mapping_pair` nodes inside
/// the top-level `block_mapping`. The tree-sitter-yaml tree structure is:
///
///   stream → document → block_node → block_mapping → block_mapping_pair
///
/// We extract each `block_mapping_pair` whose parent `block_mapping` is at
/// the document level.
fn extract_yaml_recursive(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    match node.kind() {
        "block_mapping_pair" | "flow_pair" => {
            // We only want top-level pairs. Check depth: a top-level pair's
            // parent chain is block_mapping → block_node → document → stream.
            // We detect this by checking that no ancestor block_mapping_pair exists.
            if is_top_level_pair(node)
                && let Some(key_node) = node.child_by_field_name("key")
            {
                let key = node_text(key_node, source).trim().to_string();
                if !key.is_empty() {
                    push_symbol(
                        symbols,
                        "document",
                        &key,
                        SymbolKind::Section,
                        node,
                        source,
                        file_path,
                    );
                }
            }
            // Don't recurse into children — nested pairs are part of this symbol's body.
            return;
        }
        _ => {}
    }

    // Recurse into all children.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        extract_yaml_recursive(child, source, file_path, symbols);
    }
}

/// Check whether a `block_mapping_pair` is at the document's top level by
/// walking ancestors. If we hit another `block_mapping_pair` before reaching
/// the document root, this is a nested pair.
fn is_top_level_pair(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "block_mapping_pair" || parent.kind() == "flow_pair" {
            return false;
        }
        current = parent.parent();
    }
    true
}

#[cfg(test)]
#[path = "yaml_tests.rs"]
mod tests;
