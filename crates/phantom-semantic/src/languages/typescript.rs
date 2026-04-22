//! TypeScript/JavaScript symbol and reference extraction via `tree-sitter-typescript`.
//!
//! Supports both `.ts`/`.js` (TypeScript grammar) and `.tsx`/`.jsx` (TSX grammar).
//! Produces signature-hashed symbols so dependents don't see body-only edits
//! as breaking, and extracts references (calls, new-expressions, imports,
//! type references, heritage clauses) for the dependency graph.

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

    fn extract_references(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
        symbols: &[SymbolEntry],
    ) -> Vec<SymbolReference> {
        let mut refs = Vec::new();
        walk_ts_references(tree.root_node(), source, file_path, symbols, &mut refs);
        refs
    }
}

// ── Symbol extraction ────────────────────────────────────────────────────

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
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, ROOT_SCOPE);
                let signature = function_like_signature_bytes(node, source);
                push_symbol_with_signature(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Function,
                    node,
                    source,
                    file_path,
                    signature,
                );
            }
        }
        "class_declaration" => {
            let Some(name) = child_field_text(node, "name", source) else {
                return;
            };
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            let signature = class_signature_bytes(node, source);
            push_symbol_with_signature(
                symbols,
                &scope,
                &name,
                SymbolKind::Class,
                node,
                source,
                file_path,
                signature,
            );
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
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, ROOT_SCOPE);
                let signature = function_like_signature_bytes(node, source);
                push_symbol_with_signature(
                    symbols,
                    &scope,
                    &name,
                    SymbolKind::Method,
                    node,
                    source,
                    file_path,
                    signature,
                );
            }
        }
        "import_statement" => {
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
        "export_statement" => {
            // Check if it has a declaration child — extract that instead
            if let Some(decl) = node.child_by_field_name("declaration") {
                extract_ts_node(decl, source, file_path, scope_parts, symbols);
            } else {
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

/// Return the bytes of the declaration portion (up to the body) for
/// `function_declaration` and `method_definition` nodes.
fn function_like_signature_bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    &source[start..end.min(source.len())]
}

/// Return the signature portion of a class declaration — everything before
/// the body (so `class Foo extends Bar implements Iface` but not the method
/// list).
fn class_signature_bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    &source[start..end.min(source.len())]
}

// ── Reference extraction ─────────────────────────────────────────────────

fn walk_ts_references(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    refs: &mut Vec<SymbolReference>,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(function) = node.child_by_field_name("function")
                && let Some((name, scope_hint)) = resolve_ts_callee(function, source)
            {
                push_ts_reference(
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
        "new_expression" => {
            if let Some(constructor) = node.child_by_field_name("constructor")
                && let Some((name, scope_hint)) = resolve_ts_type_ref(constructor, source)
            {
                // Construction is a call-like reference.
                push_ts_reference(
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
        "import_statement" => {
            walk_ts_import(node, source, file_path, symbols, refs);
        }
        "extends_clause" | "implements_clause" => {
            // Heritage clauses inside a class declaration.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some((name, scope_hint)) = resolve_ts_type_ref(child, source) {
                    let kind = if node.kind() == "implements_clause" {
                        ReferenceKind::TraitImpl
                    } else {
                        ReferenceKind::TypeUse
                    };
                    push_ts_reference(refs, symbols, child, kind, name, scope_hint, file_path);
                }
            }
        }
        "type_identifier" | "nested_type_identifier" | "generic_type" => {
            if !ts_is_definition_name(node)
                && let Some((name, scope_hint)) = resolve_ts_type_ref(node, source)
            {
                push_ts_reference(
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
        walk_ts_references(child, source, file_path, symbols, refs);
    }
}

fn walk_ts_import(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    refs: &mut Vec<SymbolReference>,
) {
    // `import_clause` contains the bindings (named/default/namespace).
    let Some(clause) = node
        .named_children(&mut node.walk())
        .find(|c| c.kind() == "import_clause")
    else {
        return;
    };
    walk_ts_import_clause(clause, source, file_path, symbols, refs);
}

fn walk_ts_import_clause(
    clause: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    refs: &mut Vec<SymbolReference>,
) {
    let mut cursor = clause.walk();
    for child in clause.named_children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                // Default import: `import Foo from '...'`.
                push_ts_reference(
                    refs,
                    symbols,
                    child,
                    ReferenceKind::Import,
                    node_text(child, source),
                    None,
                    file_path,
                );
            }
            "named_imports" => {
                let mut c = child.walk();
                for spec in child.named_children(&mut c) {
                    if spec.kind() == "import_specifier" {
                        let name_node = spec
                            .child_by_field_name("name")
                            .or_else(|| spec.named_child(0));
                        if let Some(n) = name_node {
                            push_ts_reference(
                                refs,
                                symbols,
                                n,
                                ReferenceKind::Import,
                                node_text(n, source),
                                None,
                                file_path,
                            );
                        }
                    }
                }
            }
            "namespace_import" => {
                // `import * as foo from '...'` — bind the alias name.
                if let Some(name_node) = child.child_by_field_name("name").or_else(|| {
                    let mut c = child.walk();
                    child
                        .named_children(&mut c)
                        .find(|n| n.kind() == "identifier")
                }) {
                    push_ts_reference(
                        refs,
                        symbols,
                        name_node,
                        ReferenceKind::Import,
                        node_text(name_node, source),
                        None,
                        file_path,
                    );
                }
            }
            _ => {}
        }
    }
}

fn resolve_ts_callee(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "identifier" | "property_identifier" => Some((node_text(node, source), None)),
        "member_expression" => {
            // `x.method()` — extract the property name.
            let property = node.child_by_field_name("property")?;
            let object_text = node
                .child_by_field_name("object")
                .map(|o| node_text(o, source));
            Some((node_text(property, source), object_text))
        }
        _ => None,
    }
}

fn resolve_ts_type_ref(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "type_identifier" | "identifier" => Some((node_text(node, source), None)),
        "nested_type_identifier" => {
            // `Namespace.Type` → name = "Type", scope_hint = "Namespace".
            let name_node = node.child_by_field_name("name")?;
            let module_node = node.child_by_field_name("module");
            Some((
                node_text(name_node, source),
                module_node.map(|m| node_text(m, source)),
            ))
        }
        "generic_type" => {
            // `Array<T>` → unwrap to the inner type.
            node.child_by_field_name("name")
                .and_then(|n| resolve_ts_type_ref(n, source))
        }
        _ => None,
    }
}

/// `true` if this node is the `name` field of a declaration node — avoid
/// treating `interface Foo {}` as a type-use reference to `Foo`.
fn ts_is_definition_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    matches!(
        parent.kind(),
        "class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
    ) && parent
        .child_by_field_name("name")
        .is_some_and(|n| n.id() == node.id())
}

fn push_ts_reference(
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
        .map_or_else(|| ts_file_module_id(file_path), |s| s.id.clone());

    // Drop self-references (function calling itself).
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

fn ts_file_module_id(file_path: &Path) -> SymbolId {
    SymbolId(format!(
        "{ROOT_SCOPE}::__file__::{}::module",
        file_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ts(source: &str) -> (Vec<SymbolEntry>, Vec<SymbolReference>) {
        let mut parser = tree_sitter::Parser::new();
        let extractor = TypeScriptExtractor::new();
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let syms = extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.ts"));
        let refs =
            extractor.extract_references(&tree, source.as_bytes(), Path::new("test.ts"), &syms);
        (syms, refs)
    }

    fn parse_tsx(source: &str) -> (Vec<SymbolEntry>, Vec<SymbolReference>) {
        let mut parser = tree_sitter::Parser::new();
        let extractor = TypeScriptExtractor::tsx();
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let syms = extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.tsx"));
        let refs =
            extractor.extract_references(&tree, source.as_bytes(), Path::new("test.tsx"), &syms);
        (syms, refs)
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
        let (symbols, _) = parse_ts(src);
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
        let (symbols, _) = parse_ts(src);
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

    #[test]
    fn signature_hash_stable_for_body_only_change() {
        let (s1, _) = parse_ts("function foo(x: number): number { return x; }");
        let (s2, _) = parse_ts("function foo(x: number): number { return x + 1; }");
        assert_eq!(s1[0].name, s2[0].name);
        assert_eq!(
            s1[0].signature_hash, s2[0].signature_hash,
            "body-only change must not alter the signature hash"
        );
        assert_ne!(s1[0].content_hash, s2[0].content_hash);
    }

    #[test]
    fn signature_hash_changes_when_parameters_change() {
        let (s1, _) = parse_ts("function foo(x: number): number { return x; }");
        let (s2, _) = parse_ts("function foo(x: number, y: number): number { return x + y; }");
        assert_ne!(s1[0].signature_hash, s2[0].signature_hash);
    }

    #[test]
    fn signature_hash_changes_when_return_type_changes() {
        let (s1, _) = parse_ts("function foo(): number { return 0; }");
        let (s2, _) = parse_ts("function foo(): string { return '0'; }");
        assert_ne!(s1[0].signature_hash, s2[0].signature_hash);
    }

    #[test]
    fn captures_function_call() {
        let (_, refs) = parse_ts(
            r#"
function login() {}
function caller() {
    login();
}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::Call && r.target_name == "login")
        );
    }

    #[test]
    fn captures_method_call_with_scope_hint() {
        let (_, refs) = parse_ts(
            r#"
function run() {
    authService.login();
}
"#,
        );
        let login = refs
            .iter()
            .find(|r| r.target_name == "login" && r.kind == ReferenceKind::Call)
            .expect("expected method call to login");
        assert_eq!(login.target_scope_hint.as_deref(), Some("authService"));
    }

    #[test]
    fn captures_new_expression() {
        let (_, refs) = parse_ts(
            r#"
class User {}
function make() {
    new User();
}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::Call && r.target_name == "User"),
            "expected construction of User to be captured"
        );
    }

    #[test]
    fn captures_named_imports() {
        let (_, refs) = parse_ts(r#"import { useState, useEffect } from 'react';"#);
        let names: Vec<&str> = refs
            .iter()
            .filter(|r| r.kind == ReferenceKind::Import)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(names.contains(&"useState"));
        assert!(names.contains(&"useEffect"));
    }

    #[test]
    fn captures_extends_clause() {
        let (_, refs) = parse_ts(
            r#"
class Base {}
class Derived extends Base {}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::TypeUse && r.target_name == "Base"),
            "expected extends clause to produce a TypeUse ref to Base, got: {refs:?}"
        );
    }

    #[test]
    fn captures_implements_clause() {
        let (_, refs) = parse_ts(
            r#"
interface Greet {}
class Person implements Greet {}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::TraitImpl && r.target_name == "Greet"),
            "expected implements clause → TraitImpl ref to Greet"
        );
    }

    #[test]
    fn captures_type_annotation() {
        let (_, refs) = parse_ts(
            r#"
interface Order {}
function place(o: Order): void {}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::TypeUse && r.target_name == "Order"),
            "expected type annotation to produce TypeUse ref to Order"
        );
    }

    #[test]
    fn self_recursion_does_not_emit_self_edge() {
        let (_, refs) =
            parse_ts("function fib(n: number): number { return n < 2 ? n : fib(n-1) + fib(n-2); }");
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
    fn export_function_call_is_attributed_to_enclosing_function() {
        // When an `export function` wraps a declaration, the call inside the
        // body should still be attributed to the inner function, not to the
        // surrounding import statement.
        let src = r#"import { login } from './auth';

export function handleRequest(): boolean {
    return login(42);
}
"#;
        let (_, refs) = parse_ts(src);
        let call = refs
            .iter()
            .find(|r| r.target_name == "login" && r.kind == ReferenceKind::Call)
            .expect("expected call ref for login");
        assert!(
            call.source.0.contains("handleRequest"),
            "source should attribute to handleRequest, got: {}",
            call.source.0
        );
    }

    #[test]
    fn tsx_extractor_captures_component_refs() {
        let (_, refs) = parse_tsx(
            r#"
function Button(): JSX.Element { return <div />; }
function App(): JSX.Element {
    return Button();
}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::Call && r.target_name == "Button"),
            "TSX extractor must capture component calls"
        );
    }
}
