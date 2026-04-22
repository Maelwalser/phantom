//! Go symbol and reference extraction via `tree-sitter-go`.

use std::path::Path;

use phantom_core::id::SymbolId;
use phantom_core::symbol::{
    ReferenceKind, SymbolEntry, SymbolKind, SymbolReference, find_enclosing_symbol,
};
use tree_sitter::Node;

use super::{
    LanguageExtractor, child_field_text, for_each_named_child, node_text, push_symbol,
    push_symbol_with_signature,
};

const ROOT_SCOPE: &str = "package";

/// Extracts symbols and references from Go source files.
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
        let package_scope = resolve_package_scope(root, source);
        extract_go_node(root, source, file_path, &package_scope, &mut symbols);
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
        let package_scope = resolve_package_scope(tree.root_node(), source);
        walk_go_references(
            tree.root_node(),
            source,
            file_path,
            symbols,
            &package_scope,
            &mut refs,
        );
        refs
    }
}

/// Resolve the package scope for a Go file from its `package_clause`.
///
/// Returns the declared package name (e.g. `"auth"` for `package auth`),
/// falling back to the literal [`ROOT_SCOPE`] when no `package_clause` is
/// present. This lets callers using `auth.Login()` match against symbols
/// declared in a file that begins with `package auth`.
///
/// The tree-sitter-go grammar does not expose the package name under a
/// field name — we walk the `package_clause`'s named children looking for
/// a `package_identifier` or `_package_identifier` token.
fn resolve_package_scope(root: Node<'_>, source: &[u8]) -> String {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() != "package_clause" {
            continue;
        }
        let mut inner = child.walk();
        for ident in child.named_children(&mut inner) {
            if matches!(
                ident.kind(),
                "package_identifier" | "_package_identifier" | "identifier"
            ) {
                return node_text(ident, source);
            }
        }
    }
    ROOT_SCOPE.to_string()
}

// ── Symbol extraction ────────────────────────────────────────────────────

fn extract_go_node(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    package_scope: &str,
    symbols: &mut Vec<SymbolEntry>,
) {
    let kind = node.kind();
    match kind {
        "function_declaration" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let signature = go_function_signature_bytes(node, source);
                push_symbol_with_signature(
                    symbols,
                    package_scope,
                    &name,
                    SymbolKind::Function,
                    node,
                    source,
                    file_path,
                    signature,
                );
            }
        }
        "method_declaration" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = method_receiver_scope(node, source, package_scope);
                let signature = go_function_signature_bytes(node, source);
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
        "type_declaration" => {
            // type_declaration contains type_spec children
            for_each_named_child(node, |child| {
                if child.kind() == "type_spec"
                    && let Some(name) = child_field_text(child, "name", source)
                {
                    let sym_kind = type_spec_kind(child);
                    // For structs and interfaces, the declaration IS the
                    // signature — any change is breaking for dependents.
                    // Using plain push_symbol keeps signature_hash == content_hash.
                    push_symbol(
                        symbols,
                        package_scope,
                        &name,
                        sym_kind,
                        child,
                        source,
                        file_path,
                    );
                }
            });
        }
        "import_declaration" => {
            let text = node_text(node, source);
            push_symbol(
                symbols,
                package_scope,
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
            extract_go_node(child, source, file_path, package_scope, symbols);
        });
    }
}

/// Build the scope for a Go method from its receiver type (e.g. "s *Server" → "auth::Server").
fn method_receiver_scope(method_node: Node<'_>, source: &[u8], package_scope: &str) -> String {
    let Some(params) = method_node.child_by_field_name("receiver") else {
        return package_scope.to_string();
    };
    let recv_text = node_text(params, source);
    let trimmed = recv_text.trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace());
    let type_name = trimmed
        .split_whitespace()
        .last()
        .unwrap_or(trimmed)
        .trim_start_matches('*');
    format!("{package_scope}::{type_name}")
}

/// Signature bytes for a Go `function_declaration` / `method_declaration`:
/// everything up to (but excluding) the body block.
fn go_function_signature_bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    &source[start..end.min(source.len())]
}

/// Classify a `type_spec` child of a `type_declaration` as struct, interface, or alias.
fn type_spec_kind(type_spec: Node<'_>) -> SymbolKind {
    match type_spec.child_by_field_name("type").map(|n| n.kind()) {
        Some("struct_type") => SymbolKind::Struct,
        Some("interface_type") => SymbolKind::Interface,
        _ => SymbolKind::TypeAlias,
    }
}

// ── Reference extraction ─────────────────────────────────────────────────

fn walk_go_references(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    package_scope: &str,
    refs: &mut Vec<SymbolReference>,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(function) = node.child_by_field_name("function")
                && let Some((name, scope_hint)) = resolve_go_callee(function, source)
            {
                push_go_reference(
                    refs,
                    symbols,
                    node,
                    ReferenceKind::Call,
                    name,
                    scope_hint,
                    file_path,
                    package_scope,
                );
            }
        }
        "import_spec" => {
            let path_node = node
                .child_by_field_name("path")
                .or_else(|| node.named_child(0));
            if let Some(p) = path_node {
                let text = node_text(p, source);
                // `"crate/auth/login"` → strip quotes and use the basename
                // as the target, path as scope.
                let cleaned = text.trim_matches('"');
                let (scope, name) = match cleaned.rsplit_once('/') {
                    Some((s, n)) => (Some(s.to_string()), n.to_string()),
                    None => (None, cleaned.to_string()),
                };
                push_go_reference(
                    refs,
                    symbols,
                    p,
                    ReferenceKind::Import,
                    name,
                    scope,
                    file_path,
                    package_scope,
                );
            }
        }
        "type_identifier" | "qualified_type" => {
            if !go_is_definition_name(node)
                && let Some((name, scope_hint)) = resolve_go_type_ref(node, source)
            {
                push_go_reference(
                    refs,
                    symbols,
                    node,
                    ReferenceKind::TypeUse,
                    name,
                    scope_hint,
                    file_path,
                    package_scope,
                );
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_go_references(child, source, file_path, symbols, package_scope, refs);
    }
}

fn resolve_go_callee(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "identifier" => Some((node_text(node, source), None)),
        "selector_expression" => {
            // `pkg.Fn()` or `recv.Method()` — extract the selected field.
            let field = node.child_by_field_name("field")?;
            let operand = node
                .child_by_field_name("operand")
                .map(|o| node_text(o, source));
            Some((node_text(field, source), operand))
        }
        _ => None,
    }
}

fn resolve_go_type_ref(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "type_identifier" | "identifier" => Some((node_text(node, source), None)),
        "qualified_type" => {
            // `pkg.Type`
            let package = node.child_by_field_name("package");
            let name_node = node.child_by_field_name("name")?;
            Some((
                node_text(name_node, source),
                package.map(|p| node_text(p, source)),
            ))
        }
        "pointer_type" | "slice_type" | "array_type" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .find_map(|c| resolve_go_type_ref(c, source))
        }
        _ => None,
    }
}

fn go_is_definition_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "type_spec"
        && parent
            .child_by_field_name("name")
            .is_some_and(|n| n.id() == node.id())
}

#[allow(clippy::too_many_arguments)]
fn push_go_reference(
    refs: &mut Vec<SymbolReference>,
    symbols: &[SymbolEntry],
    node: Node<'_>,
    kind: ReferenceKind,
    target_name: String,
    target_scope_hint: Option<String>,
    file_path: &Path,
    package_scope: &str,
) {
    let range = node.start_byte()..node.end_byte();
    let source = find_enclosing_symbol(symbols, &range).map_or_else(
        || go_file_module_id(file_path, package_scope),
        |s| s.id.clone(),
    );

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

fn go_file_module_id(file_path: &Path, package_scope: &str) -> SymbolId {
    SymbolId(format!(
        "{package_scope}::__file__::{}::module",
        file_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_go(source: &str) -> (Vec<SymbolEntry>, Vec<SymbolReference>) {
        let mut parser = tree_sitter::Parser::new();
        let extractor = GoExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let syms = extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.go"));
        let refs =
            extractor.extract_references(&tree, source.as_bytes(), Path::new("test.go"), &syms);
        (syms, refs)
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
        let (symbols, _) = parse_go(src);
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
        let (symbols, _) = parse_go(src);
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Interface && s.name == "Handler")
        );
    }

    #[test]
    fn signature_hash_stable_for_body_only_change() {
        let (s1, _) = parse_go("package p\nfunc Foo(x int) int {\n    return x\n}\n");
        let (s2, _) = parse_go("package p\nfunc Foo(x int) int {\n    return x + 1\n}\n");
        assert_eq!(s1[0].signature_hash, s2[0].signature_hash);
        assert_ne!(s1[0].content_hash, s2[0].content_hash);
    }

    #[test]
    fn signature_hash_changes_when_parameters_change() {
        let (s1, _) = parse_go("package p\nfunc Foo(x int) int { return x }\n");
        let (s2, _) = parse_go("package p\nfunc Foo(x, y int) int { return x + y }\n");
        assert_ne!(s1[0].signature_hash, s2[0].signature_hash);
    }

    #[test]
    fn captures_function_call() {
        let (_, refs) = parse_go(
            r#"
package p

func login() {}

func caller() {
    login()
}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::Call && r.target_name == "login"),
            "expected a Call ref to login, got: {refs:?}"
        );
    }

    #[test]
    fn captures_package_qualified_call() {
        let (_, refs) = parse_go(
            r#"
package p

func run() {
    auth.Login()
}
"#,
        );
        let login = refs
            .iter()
            .find(|r| r.target_name == "Login" && r.kind == ReferenceKind::Call)
            .expect("expected selector call to Login");
        assert_eq!(login.target_scope_hint.as_deref(), Some("auth"));
    }

    #[test]
    fn captures_type_use_in_parameters() {
        let (_, refs) = parse_go(
            r#"
package p

type Order struct{}

func place(o Order) {}
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::TypeUse && r.target_name == "Order"),
            "expected TypeUse ref to Order, got: {refs:?}"
        );
    }

    #[test]
    fn captures_qualified_type_use() {
        let (_, refs) = parse_go(
            r#"
package p

import "io"

func handle(r io.Reader) {}
"#,
        );
        let reader = refs
            .iter()
            .find(|r| r.target_name == "Reader" && r.kind == ReferenceKind::TypeUse)
            .expect("expected qualified type Reader");
        assert_eq!(reader.target_scope_hint.as_deref(), Some("io"));
    }

    #[test]
    fn captures_import_spec_with_path() {
        let (_, refs) = parse_go(
            r#"
package p

import (
    "github.com/example/auth"
)
"#,
        );
        let auth = refs
            .iter()
            .find(|r| r.kind == ReferenceKind::Import && r.target_name == "auth")
            .expect("expected import ref for auth");
        assert_eq!(
            auth.target_scope_hint.as_deref(),
            Some("github.com/example")
        );
    }

    #[test]
    fn self_recursion_does_not_emit_self_edge() {
        let (_, refs) = parse_go(
            r#"
package p

func fib(n int) int {
    if n < 2 { return n }
    return fib(n-1) + fib(n-2)
}
"#,
        );
        assert!(
            !refs
                .iter()
                .any(|r| r.kind == ReferenceKind::Call && r.target_name == "fib"),
            "expected no self-edges, got: {refs:?}"
        );
    }
}
