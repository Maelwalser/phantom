//! Go symbol extraction via `tree-sitter-go`.

use std::path::Path;

use phantom_core::id::{ContentHash, SymbolId};
use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::LanguageExtractor;

/// Extracts symbols from Go source files.
pub struct GoExtractor;

impl LanguageExtractor for GoExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["go"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_go_node(root, source, file_path, &mut symbols);
        symbols
    }
}

fn extract_go_node(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name) = child_field_text(node, "name", source) {
                push_symbol(
                    symbols,
                    "package",
                    &name,
                    SymbolKind::Function,
                    node,
                    source,
                    file_path,
                );
            }
        }
        "method_declaration" => {
            if let Some(name) = child_field_text(node, "name", source) {
                // Extract receiver type for scope
                let scope = if let Some(params) = node.child_by_field_name("receiver") {
                    let recv_text = node_text(params, source)
                        .trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace())
                        .to_string();
                    // Extract type name from receiver (e.g. "s *Server" -> "Server")
                    let type_name = recv_text
                        .split_whitespace()
                        .last()
                        .unwrap_or(&recv_text)
                        .trim_start_matches('*');
                    format!("package::{type_name}")
                } else {
                    "package".to_string()
                };
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Method,
                    node,
                    source,
                    file_path,
                );
            }
        }
        "type_declaration" => {
            // type_declaration contains type_spec children
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "type_spec"
                    && let Some(name) = child_field_text(child, "name", source)
                {
                    let type_node = child.child_by_field_name("type");
                    let sym_kind = match type_node.map(|n| n.kind()) {
                        Some("struct_type") => SymbolKind::Struct,
                        Some("interface_type") => SymbolKind::Interface,
                        _ => SymbolKind::TypeAlias,
                    };
                    push_symbol(
                        symbols, "package", &name, sym_kind, child, source, file_path,
                    );
                }
            }
        }
        "import_declaration" => {
            let text = node_text(node, source);
            push_symbol(
                symbols,
                "package",
                &text,
                SymbolKind::Import,
                node,
                source,
                file_path,
            );
        }
        _ => {}
    }

    // Recurse into top-level
    if matches!(kind, "source_file") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            extract_go_node(child, source, file_path, symbols);
        }
    }
}

fn child_field_text(node: Node<'_>, field: &str, source: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    child.utf8_text(source).ok().map(|s| s.to_string())
}

fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").to_string()
}

fn push_symbol(
    symbols: &mut Vec<SymbolEntry>,
    scope: &str,
    name: &str,
    kind: SymbolKind,
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
) {
    let kind_str = format!("{kind:?}").to_lowercase();
    let id = SymbolId(format!("{scope}::{name}::{kind_str}"));
    let content = &source[node.start_byte()..node.end_byte()];
    let content_hash = ContentHash::from_bytes(content);

    symbols.push(SymbolEntry {
        id,
        kind,
        name: name.to_string(),
        scope: scope.to_string(),
        file: file_path.to_path_buf(),
        byte_range: node.start_byte()..node.end_byte(),
        content_hash,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_go(source: &str) -> Vec<SymbolEntry> {
        let mut parser = tree_sitter::Parser::new();
        let extractor = GoExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.go"))
    }

    #[test]
    fn extracts_functions_and_structs() {
        let src = r#"
package main

func main() {
    fmt.Println("hello")
}

type Server struct {
    port int
}

func (s *Server) Start() error {
    return nil
}
"#;
        let symbols = parse_go(src);
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Function && s.name == "main")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Struct && s.name == "Server")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Method && s.name == "Start")
        );
    }

    #[test]
    fn extracts_interface_and_imports() {
        let src = r#"
package main

import "fmt"

type Handler interface {
    Handle() error
}
"#;
        let symbols = parse_go(src);
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Interface && s.name == "Handler")
        );
    }
}
