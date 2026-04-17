//! JSON symbol extraction via `tree-sitter-json`.
//!
//! Top-level object keys become `Section` symbols, enabling semantic merge when
//! two agents modify different keys in the same JSON file.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, for_each_named_child, node_text, push_symbol};

const ROOT_SCOPE: &str = "root";

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
    for_each_named_child(node, |child| match child.kind() {
        "pair" => {
            if let Some(key_node) = child.child_by_field_name("key") {
                let raw = node_text(key_node, source);
                // Strip surrounding quotes from the key.
                let key = raw.trim().trim_matches('"');
                if !key.is_empty() {
                    push_symbol(
                        symbols,
                        ROOT_SCOPE,
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
        "object" => extract_json_top_level(child, source, file_path, symbols),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_json(source: &str) -> Vec<SymbolEntry> {
        let mut parser = tree_sitter::Parser::new();
        let extractor = JsonExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("package.json"))
    }

    #[test]
    fn extracts_top_level_keys() {
        let src = r#"{
      "name": "my-app",
      "version": "1.0.0",
      "scripts": {
        "build": "tsc",
        "test": "jest"
      },
      "dependencies": {
        "react": "^18.0.0"
      }
    }"#;
        let symbols = parse_json(src);
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == "name")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == "version")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == "scripts")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == "dependencies")
        );
    }

    #[test]
    fn handles_empty_object() {
        let src = "{}";
        let symbols = parse_json(src);
        assert!(symbols.is_empty());
    }

    #[test]
    fn tsconfig_keys() {
        let src = r#"{
      "compilerOptions": {
        "target": "es2020"
      },
      "include": ["src"]
    }"#;
        let symbols = parse_json(src);
        assert!(symbols.iter().any(|s| s.name == "compilerOptions"));
        assert!(symbols.iter().any(|s| s.name == "include"));
    }
}
