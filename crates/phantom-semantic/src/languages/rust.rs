//! Rust symbol extraction via `tree-sitter-rust`.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, build_scope, child_field_text, node_text, push_symbol};

/// Extracts symbols from Rust source files.
pub struct RustExtractor;

impl LanguageExtractor for RustExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_from_node(root, source, file_path, &[], &mut symbols);
        symbols
    }
}

/// Recursively extract symbols from a tree-sitter node and its children.
fn extract_from_node(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    scope_parts: &[String],
    symbols: &mut Vec<SymbolEntry>,
) {
    let kind = node.kind();
    match kind {
        "function_item" => {
            if is_test_function(node, source) {
                if let Some(name) = child_field_text(node, "name", source) {
                    let scope = build_scope(scope_parts, "crate");
                    push_symbol(
                        symbols,
                        &scope,
                        &name,
                        SymbolKind::Test,
                        node,
                        source,
                        file_path,
                    );
                }
            } else if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                let kind = resolve_kind(SymbolKind::Function, node);
                push_symbol(symbols, &scope, &name, kind, node, source, file_path);
            }
        }
        "struct_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Struct,
                    node,
                    source,
                    file_path,
                );
            }
        }
        "enum_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Enum,
                    node,
                    source,
                    file_path,
                );
            }
        }
        "trait_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Trait,
                    node,
                    source,
                    file_path,
                );
            }
        }
        "impl_item" => {
            let impl_name = extract_impl_name(node, source);
            let scope = build_scope(scope_parts, "crate");
            push_symbol(
                symbols,
                &scope,
                &impl_name,
                SymbolKind::Impl,
                node,
                source,
                file_path,
            );
            // Recurse into impl body for methods
            if let Some(body) = node.child_by_field_name("body") {
                let mut new_scope = scope_parts.to_vec();
                new_scope.push(impl_name);
                let mut cursor = body.walk();
                for child in body.named_children(&mut cursor) {
                    extract_from_node(child, source, file_path, &new_scope, symbols);
                }
            }
            return; // Already handled children
        }
        "use_declaration" => {
            let text = node_text(node, source);
            let scope = build_scope(scope_parts, "crate");
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
        "const_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Const,
                    node,
                    source,
                    file_path,
                );
            }
        }
        "type_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::TypeAlias,
                    node,
                    source,
                    file_path,
                );
            }
        }
        "mod_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Module,
                    node,
                    source,
                    file_path,
                );
                // Recurse into module body
                if let Some(body) = node.child_by_field_name("body") {
                    let mut new_scope = scope_parts.to_vec();
                    new_scope.push(name);
                    let mut cursor = body.walk();
                    for child in body.named_children(&mut cursor) {
                        extract_from_node(child, source, file_path, &new_scope, symbols);
                    }
                }
            }
            return;
        }
        "macro_definition" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, "crate");
                push_symbol(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Function,
                    node,
                    source,
                    file_path,
                );
            }
        }
        _ => {}
    }

    // Recurse into top-level containers and error-recovery nodes
    if matches!(kind, "source_file" | "declaration_list" | "ERROR") {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            extract_from_node(child, source, file_path, scope_parts, symbols);
        }
    }
}

/// Extract the name for an impl block (e.g. "MyStruct" or "MyTrait for MyStruct").
fn extract_impl_name(node: Node<'_>, source: &[u8]) -> String {
    let type_name = child_field_text(node, "type", source).unwrap_or_default();
    if let Some(trait_name) = child_field_text(node, "trait", source) {
        format!("{trait_name} for {type_name}")
    } else {
        type_name
    }
}

/// Promote `Function` to `Method` if the node is inside an `impl_item`.
fn resolve_kind(kind: SymbolKind, node: Node<'_>) -> SymbolKind {
    if kind != SymbolKind::Function {
        return kind;
    }
    let mut parent = node.parent();
    while let Some(p) = parent {
        if p.kind() == "impl_item" {
            return SymbolKind::Method;
        }
        if p.kind() == "source_file" || p.kind() == "mod_item" {
            break;
        }
        parent = p.parent();
    }
    kind
}

/// Check if a function_item has a `#[test]` or `#[tokio::test]` attribute.
fn is_test_function(node: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute_item" || child.kind() == "attribute" {
            let text = node_text(child, source);
            if is_test_attribute(&text) {
                return true;
            }
        }
    }
    let mut prev = node.prev_sibling();
    while let Some(sibling) = prev {
        if sibling.kind() == "attribute_item" {
            let text = node_text(sibling, source);
            if is_test_attribute(&text) {
                return true;
            }
        } else if sibling.kind() != "line_comment" && sibling.kind() != "block_comment" {
            break;
        }
        prev = sibling.prev_sibling();
    }
    false
}

/// Check if an attribute text matches known test attributes.
fn is_test_attribute(text: &str) -> bool {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix("#[")
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    let inner = inner.trim();
    inner == "test"
        || inner.starts_with("test(")
        || inner == "tokio::test"
        || inner.starts_with("tokio::test(")
        || inner == "rstest"
        || inner.starts_with("rstest(")
        || inner.starts_with("test_case(")
}

#[cfg(test)]
#[path = "rust_tests.rs"]
mod tests;
