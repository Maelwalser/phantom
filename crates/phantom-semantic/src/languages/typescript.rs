//! TypeScript/JavaScript symbol extraction via `tree-sitter-typescript`.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{
    LanguageExtractor, build_scope, for_each_named_child, node_text, push_named_symbol,
    push_symbol,
};

const ROOT_SCOPE: &str = "module";

/// Extracts symbols from TypeScript and JavaScript source files.
pub struct TypeScriptExtractor {
    /// Whether to use TSX grammar (for `.tsx`/`.jsx` files).
    tsx: bool,
}

impl Default for TypeScriptExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeScriptExtractor {
    /// Create extractor for `.ts`/`.js` files.
    pub fn new() -> Self {
        Self { tsx: false }
    }

    /// Create extractor for `.tsx`/`.jsx` files.
    pub fn tsx() -> Self {
        Self { tsx: true }
    }
}

impl LanguageExtractor for TypeScriptExtractor {
    fn language(&self) -> tree_sitter::Language {
        if self.tsx {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    }

    fn extensions(&self) -> &[&str] {
        if self.tsx {
            &["tsx", "jsx"]
        } else {
            &["ts", "js"]
        }
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        extract_ts_node(tree.root_node(), source, file_path, &[], &mut symbols);
        symbols
    }
}

fn extract_ts_node(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    scope_parts: &[String],
    symbols: &mut Vec<SymbolEntry>,
) {
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            push_named_symbol(
                symbols,
                node,
                source,
                file_path,
                scope_parts,
                ROOT_SCOPE,
                SymbolKind::Function,
            );
        }
        "class_declaration" => {
            let Some(name) = super::child_field_text(node, "name", source) else {
                return;
            };
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            push_symbol(symbols, &scope, &name, SymbolKind::Class, node, source, file_path);
            // Recurse into class body for methods
            if let Some(body) = node.child_by_field_name("body") {
                let mut new_scope = scope_parts.to_vec();
                new_scope.push(name);
                for_each_named_child(body, |child| {
                    extract_ts_node(child, source, file_path, &new_scope, symbols);
                });
            }
            return;
        }
        "interface_declaration" => {
            push_named_symbol(
                symbols,
                node,
                source,
                file_path,
                scope_parts,
                ROOT_SCOPE,
                SymbolKind::Interface,
            );
        }
        "method_definition" => {
            push_named_symbol(
                symbols,
                node,
                source,
                file_path,
                scope_parts,
                ROOT_SCOPE,
                SymbolKind::Method,
            );
        }
        "import_statement" => {
            let text = node_text(node, source);
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            push_symbol(symbols, &scope, &text, SymbolKind::Import, node, source, file_path);
        }
        "export_statement" => {
            // Check if it has a declaration child — extract that instead
            if let Some(decl) = node.child_by_field_name("declaration") {
                extract_ts_node(decl, source, file_path, scope_parts, symbols);
            } else {
                let text = node_text(node, source);
                let scope = build_scope(scope_parts, ROOT_SCOPE);
                push_symbol(symbols, &scope, &text, SymbolKind::Import, node, source, file_path);
            }
            return;
        }
        "type_alias_declaration" => {
            push_named_symbol(
                symbols,
                node,
                source,
                file_path,
                scope_parts,
                ROOT_SCOPE,
                SymbolKind::TypeAlias,
            );
        }
        "enum_declaration" => {
            push_named_symbol(
                symbols,
                node,
                source,
                file_path,
                scope_parts,
                ROOT_SCOPE,
                SymbolKind::Enum,
            );
        }
        _ => {}
    }

    // Recurse into top-level containers
    if matches!(kind, "program" | "statement_block") {
        for_each_named_child(node, |child| {
            extract_ts_node(child, source, file_path, scope_parts, symbols);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ts(source: &str) -> Vec<SymbolEntry> {
        let mut parser = tree_sitter::Parser::new();
        let extractor = TypeScriptExtractor::new();
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.ts"))
    }

    #[test]
    fn extracts_function_and_class() {
        let src = r#"
    function greet(name: string): string {
        return `Hello, ${name}`;
    }

    class User {
        getName(): string {
            return this.name;
        }
    }
    "#;
        let symbols = parse_ts(src);
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
                .any(|s| s.kind == SymbolKind::Method && s.name == "getName")
        );
    }

    #[test]
    fn extracts_imports_and_interfaces() {
        let src = r#"
    import { useState } from 'react';

    interface Props {
        name: string;
    }

    type Result<T> = { ok: true; value: T } | { ok: false; error: Error };
    "#;
        let symbols = parse_ts(src);
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Interface && s.name == "Props")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::TypeAlias && s.name == "Result")
        );
    }
}
