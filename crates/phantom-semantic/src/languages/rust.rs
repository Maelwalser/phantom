//! Rust symbol and reference extraction via `tree-sitter-rust`.

use std::path::Path;

use phantom_core::id::SymbolId;
use phantom_core::symbol::{
    ReferenceKind, SymbolEntry, SymbolKind, SymbolReference, find_enclosing_symbol,
};
use tree_sitter::Node;

use super::{
    LanguageExtractor, build_scope, child_field_text, for_each_named_child, node_text,
    push_named_symbol, push_symbol, push_symbol_with_signature,
};

const ROOT_SCOPE: &str = "crate";

/// Extracts symbols and references from Rust source files.
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

    fn extract_references(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
        symbols: &[SymbolEntry],
    ) -> Vec<SymbolReference> {
        let mut refs = Vec::new();
        let root = tree.root_node();
        walk_references(root, source, file_path, symbols, &mut refs);
        refs
    }
}

// ── Symbol extraction ────────────────────────────────────────────────────

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
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, ROOT_SCOPE);
                let sym_kind = if is_test_function(node, source) {
                    SymbolKind::Test
                } else {
                    resolve_kind(SymbolKind::Function, node)
                };
                let signature = function_signature_bytes(node, source);
                push_symbol_with_signature(
                    symbols, &scope, &name, sym_kind, node, source, file_path, signature,
                );
            }
        }
        "struct_item" => push_named_symbol(
            symbols,
            node,
            source,
            file_path,
            scope_parts,
            ROOT_SCOPE,
            SymbolKind::Struct,
        ),
        "enum_item" => push_named_symbol(
            symbols,
            node,
            source,
            file_path,
            scope_parts,
            ROOT_SCOPE,
            SymbolKind::Enum,
        ),
        "trait_item" => push_named_symbol(
            symbols,
            node,
            source,
            file_path,
            scope_parts,
            ROOT_SCOPE,
            SymbolKind::Trait,
        ),
        "impl_item" => {
            let impl_name = extract_impl_name(node, source);
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            let signature = impl_signature_bytes(node, source);
            push_symbol_with_signature(
                symbols,
                &scope,
                &impl_name,
                SymbolKind::Impl,
                node,
                source,
                file_path,
                signature,
            );
            // Recurse into impl body for methods
            if let Some(body) = node.child_by_field_name("body") {
                let mut new_scope = scope_parts.to_vec();
                new_scope.push(impl_name);
                for_each_named_child(body, |child| {
                    extract_from_node(child, source, file_path, &new_scope, symbols);
                });
            }
            return; // Already handled children
        }
        "use_declaration" => {
            let text = node_text(node, source);
            let scope = build_scope(scope_parts, ROOT_SCOPE);
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
        "const_item" => push_named_symbol(
            symbols,
            node,
            source,
            file_path,
            scope_parts,
            ROOT_SCOPE,
            SymbolKind::Const,
        ),
        "type_item" => push_named_symbol(
            symbols,
            node,
            source,
            file_path,
            scope_parts,
            ROOT_SCOPE,
            SymbolKind::TypeAlias,
        ),
        "mod_item" => {
            let Some(name) = child_field_text(node, "name", source) else {
                return;
            };
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            push_symbol(
                symbols,
                &scope,
                &name,
                SymbolKind::Module,
                node,
                source,
                file_path,
            );
            if let Some(body) = node.child_by_field_name("body") {
                let mut new_scope = scope_parts.to_vec();
                new_scope.push(name);
                for_each_named_child(body, |child| {
                    extract_from_node(child, source, file_path, &new_scope, symbols);
                });
            }
            return;
        }
        "macro_definition" => push_named_symbol(
            symbols,
            node,
            source,
            file_path,
            scope_parts,
            ROOT_SCOPE,
            SymbolKind::Function,
        ),
        _ => {}
    }

    // Recurse into top-level containers and error-recovery nodes
    if matches!(kind, "source_file" | "declaration_list" | "ERROR") {
        for_each_named_child(node, |child| {
            extract_from_node(child, source, file_path, scope_parts, symbols);
        });
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

/// Return the signature bytes of a `function_item` node — everything from the
/// node's start up to (but excluding) the `body` field.
///
/// Used for `signature_hash` so that body-only edits don't cause dependents
/// to be flagged as breaking.
fn function_signature_bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    &source[start..end.min(source.len())]
}

/// Return the signature bytes of an `impl_item` — everything up to the body.
fn impl_signature_bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    &source[start..end.min(source.len())]
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

// ── Reference extraction ─────────────────────────────────────────────────

/// Recursively walk the tree collecting symbol references.
///
/// Captures:
/// * `call_expression` — [`ReferenceKind::Call`]
/// * `use_declaration` — [`ReferenceKind::Import`]
/// * `impl_item` with a trait field — [`ReferenceKind::TraitImpl`]
/// * `type_identifier` and `scoped_type_identifier` used in type positions —
///   [`ReferenceKind::TypeUse`]
fn walk_references(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    refs: &mut Vec<SymbolReference>,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(function) = node.child_by_field_name("function")
                && let Some((name, scope_hint)) = resolve_callee(function, source)
            {
                push_reference(
                    refs,
                    symbols,
                    node,
                    ReferenceKind::Call,
                    name,
                    scope_hint,
                    file_path,
                );
            }
        }
        "use_declaration" => {
            // Capture the imported path(s). `argument` is a
            // scoped_identifier / use_list / use_as_clause / identifier.
            if let Some(arg) = node.child_by_field_name("argument") {
                walk_use_argument(arg, source, file_path, symbols, refs);
            }
        }
        "impl_item" => {
            if let Some(trait_node) = node.child_by_field_name("trait")
                && let Some((name, scope_hint)) = resolve_type_ref(trait_node, source)
            {
                push_reference(
                    refs,
                    symbols,
                    trait_node,
                    ReferenceKind::TraitImpl,
                    name,
                    scope_hint,
                    file_path,
                );
            }
            // `type` field is the impl target — it's a type use from inside
            // the impl block, so attribute it to the impl symbol.
            if let Some(type_node) = node.child_by_field_name("type")
                && let Some((name, scope_hint)) = resolve_type_ref(type_node, source)
            {
                push_reference(
                    refs,
                    symbols,
                    type_node,
                    ReferenceKind::TypeUse,
                    name,
                    scope_hint,
                    file_path,
                );
            }
        }
        "type_identifier" | "scoped_type_identifier" => {
            // Only emit if this is not a definition's `name` field — those
            // are already captured as symbols.
            if !is_definition_name(node)
                && let Some((name, scope_hint)) = resolve_type_ref(node, source)
            {
                push_reference(
                    refs,
                    symbols,
                    node,
                    ReferenceKind::TypeUse,
                    name,
                    scope_hint,
                    file_path,
                );
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_references(child, source, file_path, symbols, refs);
    }
}

/// Walk the argument of a `use_declaration` and emit an `Import` ref per
/// imported identifier. Handles `use a::b::c`, `use a::b::{c, d}`, and
/// `use a::b as c`.
fn walk_use_argument(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    refs: &mut Vec<SymbolReference>,
) {
    match node.kind() {
        "scoped_identifier" | "identifier" => {
            if let Some((name, scope_hint)) = resolve_path(node, source) {
                push_reference(
                    refs,
                    symbols,
                    node,
                    ReferenceKind::Import,
                    name,
                    scope_hint,
                    file_path,
                );
            }
        }
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                walk_use_argument(path, source, file_path, symbols, refs);
            }
        }
        "use_list" | "scoped_use_list" => {
            // `use a::{b, c}` — recurse into list items. For `scoped_use_list`,
            // the `path` field carries the prefix and the list children are
            // the individual entries; we recurse into every named child.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_use_argument(child, source, file_path, symbols, refs);
            }
        }
        _ => {}
    }
}

/// Extract `(name, scope_hint)` from a callee node (identifier,
/// scoped_identifier, or field_expression).
fn resolve_callee(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "identifier" => Some((node_text(node, source), None)),
        "scoped_identifier" => resolve_path(node, source),
        "field_expression" => {
            // `x.method_name(...)` — extract the field name.
            node.child_by_field_name("field")
                .map(|n| (node_text(n, source), None))
        }
        _ => None,
    }
}

/// Extract `(name, scope_hint)` from a type reference node.
fn resolve_type_ref(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "type_identifier" | "identifier" => Some((node_text(node, source), None)),
        "scoped_type_identifier" | "scoped_identifier" => resolve_path(node, source),
        "generic_type" => {
            // Unwrap to the underlying type.
            node.child_by_field_name("type")
                .and_then(|n| resolve_type_ref(n, source))
        }
        "reference_type" | "pointer_type" | "array_type" | "tuple_type" | "slice_type" => {
            // Descend into element types for top-level TypeUse attribution.
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find_map(|c| resolve_type_ref(c, source))
        }
        _ => None,
    }
}

/// Split a `scoped_identifier` / `scoped_type_identifier` into `(name, scope_hint)`.
///
/// For `crate::auth::login` returns `("login", Some("crate::auth"))`.
fn resolve_path(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    if node.kind() == "identifier" {
        return Some((node_text(node, source), None));
    }
    let name = child_field_text(node, "name", source)?;
    let scope_hint = child_field_text(node, "path", source);
    Some((name, scope_hint))
}

/// Return `true` if `node` is the `name` field of its parent definition.
///
/// Used to avoid treating a definition's own name (`struct Foo {}`) as a
/// type-use reference.
fn is_definition_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    matches!(
        parent.kind(),
        "struct_item"
            | "enum_item"
            | "trait_item"
            | "type_item"
            | "union_item"
            | "enum_variant"
            | "const_item"
    ) && parent
        .child_by_field_name("name")
        .is_some_and(|n| n.id() == node.id())
}

/// Resolve the enclosing symbol for `node` and push a `SymbolReference`.
///
/// If no symbol encloses the reference (e.g. a module-level `use` statement
/// at crate root), attach a synthetic file-level module source so the
/// reference isn't lost.
fn push_reference(
    refs: &mut Vec<SymbolReference>,
    symbols: &[SymbolEntry],
    node: Node<'_>,
    kind: ReferenceKind,
    target_name: String,
    target_scope_hint: Option<String>,
    file_path: &Path,
) {
    let range = node.start_byte()..node.end_byte();
    let source = find_enclosing_symbol(symbols, &range)
        .map_or_else(|| file_module_id(file_path), |s| s.id.clone());

    // Drop self-references (a symbol calling itself). Conservative: only
    // skip when the target scope also matches (or no scope hint is given),
    // so we don't drop genuine calls to a same-named function in a
    // different scope.
    if let Some(enclosing) = find_enclosing_symbol(symbols, &range)
        && enclosing.name == target_name
        && target_scope_hint
            .as_ref()
            .is_none_or(|s| s == &enclosing.scope)
    {
        return;
    }

    refs.push(SymbolReference {
        source,
        target_name,
        target_scope_hint,
        kind,
        file: file_path.to_path_buf(),
        byte_range: range,
    });
}

/// Synthetic per-file module symbol identifier for references that don't live
/// inside any extracted symbol.
fn file_module_id(file_path: &Path) -> SymbolId {
    SymbolId(format!(
        "{ROOT_SCOPE}::__file__::{}::module",
        file_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> (tree_sitter::Tree, Vec<u8>) {
        let mut parser = tree_sitter::Parser::new();
        let extractor = RustExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        (tree, source.as_bytes().to_vec())
    }

    fn parse_and_extract(source: &str) -> Vec<SymbolEntry> {
        let (tree, bytes) = parse(source);
        RustExtractor.extract_symbols(&tree, &bytes, Path::new("test.rs"))
    }

    fn parse_and_extract_refs(source: &str) -> (Vec<SymbolEntry>, Vec<SymbolReference>) {
        let (tree, bytes) = parse(source);
        let syms = RustExtractor.extract_symbols(&tree, &bytes, Path::new("test.rs"));
        let refs = RustExtractor.extract_references(&tree, &bytes, Path::new("test.rs"), &syms);
        (syms, refs)
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

        let structs: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Struct)
            .collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name, "MyStruct");

        let impls: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Impl)
            .collect();
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].name, "MyStruct");

        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Method)
            .collect();
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
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
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
        let tests: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Test)
            .collect();
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

        let outer_fn: Vec<_> = symbols.iter().filter(|s| s.name == "outer_fn").collect();
        assert_eq!(outer_fn.len(), 1);
        assert_eq!(outer_fn[0].scope, "crate::outer");

        let inner_fn: Vec<_> = symbols.iter().filter(|s| s.name == "inner_fn").collect();
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

        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Enum && s.name == "Color")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Trait && s.name == "Drawable")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Const && s.name == "MAX_SIZE")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::TypeAlias && s.name == "Result")
        );
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

    #[test]
    fn signature_hash_stable_when_only_body_changes() {
        let src1 = "fn foo(x: i32) -> i32 { x }";
        let src2 = "fn foo(x: i32) -> i32 { x + 1 }";
        let s1 = parse_and_extract(src1);
        let s2 = parse_and_extract(src2);
        assert_eq!(s1[0].signature_hash, s2[0].signature_hash);
        assert_ne!(s1[0].content_hash, s2[0].content_hash);
    }

    #[test]
    fn signature_hash_changes_when_parameters_change() {
        let src1 = "fn foo(x: i32) -> i32 { x }";
        let src2 = "fn foo(x: i32, y: i32) -> i32 { x }";
        let s1 = parse_and_extract(src1);
        let s2 = parse_and_extract(src2);
        assert_ne!(s1[0].signature_hash, s2[0].signature_hash);
    }

    #[test]
    fn signature_hash_changes_when_return_type_changes() {
        let src1 = "fn foo() -> i32 { 0 }";
        let src2 = "fn foo() -> u32 { 0 }";
        let s1 = parse_and_extract(src1);
        let s2 = parse_and_extract(src2);
        assert_ne!(s1[0].signature_hash, s2[0].signature_hash);
    }

    // ── Reference extraction tests ──────────────────────────────────────

    #[test]
    fn captures_function_call() {
        let src = r#"
fn login() {}
fn caller() {
    login();
}
"#;
        let (_, refs) = parse_and_extract_refs(src);
        let calls: Vec<_> = refs
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call)
            .collect();
        assert!(
            calls.iter().any(|r| r.target_name == "login"),
            "expected a call to `login`, got: {refs:?}"
        );
        let call = calls.iter().find(|r| r.target_name == "login").unwrap();
        assert!(
            call.source.0.contains("caller"),
            "expected source to be `caller`, got {}",
            call.source.0
        );
    }

    #[test]
    fn captures_scoped_call_with_scope_hint() {
        let src = r#"
fn run() {
    crate::auth::login();
}
"#;
        let (_, refs) = parse_and_extract_refs(src);
        let login = refs
            .iter()
            .find(|r| r.target_name == "login")
            .expect("expected login ref");
        assert_eq!(login.target_scope_hint.as_deref(), Some("crate::auth"));
        assert_eq!(login.kind, ReferenceKind::Call);
    }

    #[test]
    fn captures_type_use_in_parameters() {
        let src = r#"
struct Order;
fn place(o: Order) {}
"#;
        let (_, refs) = parse_and_extract_refs(src);
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::TypeUse && r.target_name == "Order"),
            "expected a type-use ref to Order, got: {refs:?}"
        );
    }

    #[test]
    fn captures_import() {
        let src = r#"
use crate::auth::login;
"#;
        let (_, refs) = parse_and_extract_refs(src);
        let imports: Vec<_> = refs
            .iter()
            .filter(|r| r.kind == ReferenceKind::Import)
            .collect();
        assert!(
            imports.iter().any(|r| r.target_name == "login"),
            "expected an import ref to login, got: {refs:?}"
        );
        let login = imports.iter().find(|r| r.target_name == "login").unwrap();
        assert_eq!(login.target_scope_hint.as_deref(), Some("crate::auth"));
    }

    #[test]
    fn captures_trait_impl() {
        let src = r#"
trait Greet {}
struct Person;
impl Greet for Person {}
"#;
        let (_, refs) = parse_and_extract_refs(src);
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::TraitImpl && r.target_name == "Greet"),
            "expected a TraitImpl ref to Greet, got: {refs:?}"
        );
    }

    #[test]
    fn self_recursion_does_not_emit_self_edge() {
        let src = r#"
fn fib(n: u32) -> u32 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}
"#;
        let (_, refs) = parse_and_extract_refs(src);
        let self_calls: Vec<_> = refs
            .iter()
            .filter(|r| r.kind == ReferenceKind::Call && r.target_name == "fib")
            .collect();
        assert!(
            self_calls.is_empty(),
            "expected no self-edges, got: {self_calls:?}"
        );
    }

    #[test]
    fn no_refs_for_empty_file() {
        let (_, refs) = parse_and_extract_refs("");
        assert!(refs.is_empty());
    }

    #[test]
    fn references_attributed_to_enclosing_method() {
        let src = r#"
fn helper() {}
struct S;
impl S {
    fn do_work(&self) {
        helper();
    }
}
"#;
        let (_, refs) = parse_and_extract_refs(src);
        let call = refs
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.target_name == "helper")
            .expect("expected call to helper");
        assert!(
            call.source.0.contains("do_work"),
            "expected source to be do_work method, got {}",
            call.source.0
        );
    }
}
