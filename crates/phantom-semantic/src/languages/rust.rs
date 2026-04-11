//! Rust symbol extraction via `tree-sitter-rust`.

use std::path::Path;

use phantom_core::id::{ContentHash, SymbolId};
use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::LanguageExtractor;

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
                    let scope = build_scope(scope_parts);
                    push_symbol(symbols, &scope, &name, SymbolKind::Test, node, source, file_path);
                }
            } else if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::Function, node, source, file_path);
            }
        }
        "struct_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::Struct, node, source, file_path);
            }
        }
        "enum_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::Enum, node, source, file_path);
            }
        }
        "trait_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::Trait, node, source, file_path);
            }
        }
        "impl_item" => {
            let impl_name = extract_impl_name(node, source);
            let scope = build_scope(scope_parts);
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
            let scope = build_scope(scope_parts);
            push_symbol(symbols, &scope, &text, SymbolKind::Import, node, source, file_path);
        }
        "const_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::Const, node, source, file_path);
            }
        }
        "type_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::TypeAlias, node, source, file_path);
            }
        }
        "mod_item" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::Module, node, source, file_path);
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
                let scope = build_scope(scope_parts);
                push_symbol(symbols, &scope, &name, SymbolKind::Function, node, source, file_path);
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

/// Build scope string from parts, defaulting to "crate" for empty scope.
fn build_scope(parts: &[String]) -> String {
    if parts.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", parts.join("::"))
    }
}

/// Extract text of a child field from a node.
fn child_field_text(node: Node<'_>, field: &str, source: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    child.utf8_text(source).ok().map(|s| s.to_string())
}

/// Extract the full text of a node.
fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").to_string()
}

/// Extract the name for an impl block (e.g. "MyStruct" or "MyTrait for MyStruct").
fn extract_impl_name(node: Node<'_>, source: &[u8]) -> String {
    // impl_item has optional "trait" field and required "type" field
    let type_name = child_field_text(node, "type", source).unwrap_or_default();
    if let Some(trait_name) = child_field_text(node, "trait", source) {
        format!("{trait_name} for {type_name}")
    } else {
        type_name
    }
}

/// Check if a function_item has a `#[test]` or `#[tokio::test]` attribute.
///
/// In tree-sitter-rust, attributes are preceding siblings of the function_item,
/// not children. We check the previous sibling(s) for `attribute_item` nodes.
fn is_test_function(node: Node<'_>, source: &[u8]) -> bool {
    // Check children first (some grammar versions nest attributes)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute_item" || child.kind() == "attribute" {
            let text = node_text(child, source);
            if is_test_attribute(&text) {
                return true;
            }
        }
    }
    // Check preceding siblings (common in tree-sitter-rust)
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
///
/// Matches `#[test]`, `#[tokio::test]`, `#[rstest]`, `#[test_case(...)]`
/// but NOT `#[tested]`, `#[contest]`, etc.
fn is_test_attribute(text: &str) -> bool {
    let trimmed = text.trim();
    // Strip outer #[ ... ]
    let inner = trimmed
        .strip_prefix("#[")
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    let inner = inner.trim();
    // Check for exact matches or known prefixes
    inner == "test"
        || inner.starts_with("test(")
        || inner == "tokio::test"
        || inner.starts_with("tokio::test(")
        || inner == "rstest"
        || inner.starts_with("rstest(")
        || inner.starts_with("test_case(")
}

/// Push a symbol entry with method-detection logic.
///
/// If the function is directly inside an impl block, treat it as Method.
/// We detect this by checking if the node's parent is an `impl_item` body.
fn push_symbol(
    symbols: &mut Vec<SymbolEntry>,
    scope: &str,
    name: &str,
    mut kind: SymbolKind,
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
) {
    // Promote Function to Method only if the direct parent chain contains an impl block.
    // Check by walking tree-sitter parents, not by scope depth (which false-positives
    // on nested modules).
    if kind == SymbolKind::Function {
        let mut parent = node.parent();
        while let Some(p) = parent {
            if p.kind() == "impl_item" {
                kind = SymbolKind::Method;
                break;
            }
            // Stop at source_file or other top-level containers — don't go higher.
            if p.kind() == "source_file" || p.kind() == "mod_item" {
                break;
            }
            parent = p.parent();
        }
    }

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

    fn parse_and_extract(source: &str) -> Vec<SymbolEntry> {
        let mut parser = tree_sitter::Parser::new();
        let extractor = RustExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.rs"))
    }

    #[test]
    fn extracts_three_functions() {
        let src = r#"
fn foo() {}
fn bar() -> i32 { 42 }
fn baz(x: &str) {}
"#;
        let symbols = parse_and_extract(src);
        let fns: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .collect();
        assert_eq!(fns.len(), 3);
        assert_eq!(fns[0].name, "foo");
        assert_eq!(fns[1].name, "bar");
        assert_eq!(fns[2].name, "baz");
        for f in &fns {
            assert_eq!(f.scope, "crate");
        }
    }

    #[test]
    fn extracts_struct_and_impl_with_methods() {
        let src = r#"
struct MyStruct {
    x: i32,
}

impl MyStruct {
    fn new(x: i32) -> Self {
        Self { x }
    }

    fn value(&self) -> i32 {
        self.x
    }
}
"#;
        let symbols = parse_and_extract(src);

        let structs: Vec<_> = symbols.iter().filter(|s| s.kind == SymbolKind::Struct).collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name, "MyStruct");

        let impls: Vec<_> = symbols.iter().filter(|s| s.kind == SymbolKind::Impl).collect();
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].name, "MyStruct");

        let methods: Vec<_> = symbols.iter().filter(|s| s.kind == SymbolKind::Method).collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "new");
        assert_eq!(methods[1].name, "value");
        assert!(methods[0].scope.contains("MyStruct"));
    }

    #[test]
    fn extracts_use_declarations() {
        let src = r#"
use std::collections::HashMap;
use std::io::{self, Read};
"#;
        let symbols = parse_and_extract(src);
        let imports: Vec<_> = symbols.iter().filter(|s| s.kind == SymbolKind::Import).collect();
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn extracts_test_functions() {
        let src = r#"
fn normal_fn() {}

#[test]
fn test_foo() {
    assert!(true);
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_bar() {
        assert_eq!(1, 1);
    }
}
"#;
        let symbols = parse_and_extract(src);
        let tests: Vec<_> = symbols.iter().filter(|s| s.kind == SymbolKind::Test).collect();
        assert_eq!(tests.len(), 2);
        assert_eq!(tests[0].name, "test_foo");
        assert_eq!(tests[1].name, "test_bar");
    }

    #[test]
    fn empty_file_returns_empty() {
        let symbols = parse_and_extract("");
        assert!(symbols.is_empty());
    }

    #[test]
    fn syntax_error_still_extracts_valid_symbols() {
        let src = r#"
fn valid_fn() {}

fn broken( {

fn another_valid() -> bool { true }
"#;
        let symbols = parse_and_extract(src);
        // tree-sitter is error-tolerant; should find at least some symbols
        assert!(!symbols.is_empty());
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"valid_fn"));
    }

    #[test]
    fn nested_modules_have_correct_scope() {
        let src = r#"
mod outer {
    fn outer_fn() {}

    mod inner {
        fn inner_fn() {}
    }
}
"#;
        let symbols = parse_and_extract(src);

        let outer_mod: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Module && s.name == "outer")
            .collect();
        assert_eq!(outer_mod.len(), 1);
        assert_eq!(outer_mod[0].scope, "crate");

        let outer_fn: Vec<_> = symbols
            .iter()
            .filter(|s| s.name == "outer_fn")
            .collect();
        assert_eq!(outer_fn.len(), 1);
        assert_eq!(outer_fn[0].scope, "crate::outer");

        let inner_fn: Vec<_> = symbols
            .iter()
            .filter(|s| s.name == "inner_fn")
            .collect();
        assert_eq!(inner_fn.len(), 1);
        assert_eq!(inner_fn[0].scope, "crate::outer::inner");
    }

    #[test]
    fn extracts_enum_trait_const_type_alias() {
        let src = r#"
enum Color { Red, Green, Blue }
trait Drawable { fn draw(&self); }
const MAX_SIZE: usize = 100;
type Result<T> = std::result::Result<T, MyError>;
"#;
        let symbols = parse_and_extract(src);

        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Enum && s.name == "Color"));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Trait && s.name == "Drawable"));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Const && s.name == "MAX_SIZE"));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::TypeAlias && s.name == "Result"));
    }

    #[test]
    fn symbol_id_format_is_correct() {
        let src = "fn hello() {}";
        let symbols = parse_and_extract(src);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].id.0, "crate::hello::function");
    }

    #[test]
    fn content_hash_changes_with_body() {
        let src1 = "fn foo() { 1 }";
        let src2 = "fn foo() { 2 }";
        let s1 = parse_and_extract(src1);
        let s2 = parse_and_extract(src2);
        assert_eq!(s1[0].name, s2[0].name);
        assert_ne!(s1[0].content_hash, s2[0].content_hash);
    }
}
