//! TOML symbol extraction via `tree-sitter-toml-ng`.
//!
//! TOML tables become `Section` symbols and root-level key-value pairs become
//! `Entry` symbols, enabling semantic merge of independent config sections.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, for_each_named_child, node_text, push_symbol};

const ROOT_SCOPE: &str = "root";

/// Extracts symbols from TOML files.
pub struct TomlExtractor;

impl LanguageExtractor for TomlExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_toml_ng::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["toml"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_toml_root(root, source, file_path, &mut symbols);
        symbols
    }
}

fn extract_toml_root(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    for_each_named_child(node, |child| match child.kind() {
        "table" | "table_array_element" => {
            let name = first_key_text(child, source)
                .unwrap_or_else(|| node_text(child, source).trim().to_string());
            if !name.is_empty() {
                push_symbol(
                    symbols,
                    ROOT_SCOPE,
                    &name,
                    SymbolKind::Section,
                    child,
                    source,
                    file_path,
                );
            }
        }
        "pair" => {
            if let Some(key) = first_key_text(child, source) {
                push_symbol(
                    symbols,
                    ROOT_SCOPE,
                    &key,
                    SymbolKind::Section,
                    child,
                    source,
                    file_path,
                );
            }
        }
        _ => {}
    });
}

/// Extract the first named child whose kind contains `"key"` (e.g. `bare_key`,
/// `dotted_key`, `quoted_key`). Returns `None` if no such child exists or if
/// its text is empty.
fn first_key_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind().contains("key") {
            let text = node_text(child, source).trim().to_string();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_toml(source: &str) -> Vec<SymbolEntry> {
        let mut parser = tree_sitter::Parser::new();
        let extractor = TomlExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("Cargo.toml"))
    }

    #[test]
    fn extracts_tables_and_root_pairs() {
        let src = r#"
    [package]
    name = "phantom"
    version = "0.1.0"

    [dependencies]
    serde = "1"

    [dev-dependencies]
    tempfile = "3"
    "#;
        let symbols = parse_toml(src);
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name.contains("package"))
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name.contains("dependencies"))
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name.contains("dev-dependencies"))
        );
    }

    #[test]
    fn extracts_dotted_tables() {
        let src = r#"
    [workspace.dependencies]
    serde = { version = "1", features = ["derive"] }
    "#;
        let symbols = parse_toml(src);
        assert!(!symbols.is_empty());
        // The table should be extracted as a section.
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section));
    }

    #[test]
    fn extracts_bare_root_keys() {
        let src = r#"
    name = "test"
    version = "1.0"
    "#;
        let symbols = parse_toml(src);
        assert!(symbols.iter().any(|s| s.name == "name"));
        assert!(symbols.iter().any(|s| s.name == "version"));
    }
}
