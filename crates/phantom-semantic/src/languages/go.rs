//! Go symbol extraction via `tree-sitter-go`.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, child_field_text, for_each_named_child, node_text, push_symbol};

const ROOT_SCOPE: &str = "package";

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
                    ROOT_SCOPE,
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
                let scope = method_receiver_scope(node, source);
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
            for_each_named_child(node, |child| {
                if child.kind() == "type_spec"
                    && let Some(name) = child_field_text(child, "name", source)
                {
                    let sym_kind = type_spec_kind(child);
                    push_symbol(
                        symbols, ROOT_SCOPE, &name, sym_kind, child, source, file_path,
                    );
                }
            });
        }
        "import_declaration" => {
            let text = node_text(node, source);
            push_symbol(
                symbols,
                ROOT_SCOPE,
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
        for_each_named_child(node, |child| {
            extract_go_node(child, source, file_path, symbols);
        });
    }
}

/// Build the scope for a Go method from its receiver type (e.g. "s *Server" → "package::Server").
fn method_receiver_scope(method_node: Node<'_>, source: &[u8]) -> String {
    let Some(params) = method_node.child_by_field_name("receiver") else {
        return ROOT_SCOPE.to_string();
    };
    let recv_text = node_text(params, source);
    let trimmed = recv_text.trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace());
    let type_name = trimmed
        .split_whitespace()
        .last()
        .unwrap_or(trimmed)
        .trim_start_matches('*');
    format!("{ROOT_SCOPE}::{type_name}")
}

/// Classify a `type_spec` child of a `type_declaration` as struct, interface, or alias.
fn type_spec_kind(type_spec: Node<'_>) -> SymbolKind {
    match type_spec.child_by_field_name("type").map(|n| n.kind()) {
        Some("struct_type") => SymbolKind::Struct,
        Some("interface_type") => SymbolKind::Interface,
        _ => SymbolKind::TypeAlias,
    }
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
