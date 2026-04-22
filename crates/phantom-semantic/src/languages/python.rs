//! Python symbol and reference extraction via `tree-sitter-python`.

use std::path::Path;

use phantom_core::id::SymbolId;
use phantom_core::symbol::{
    ReferenceKind, SymbolEntry, SymbolKind, SymbolReference, find_enclosing_symbol,
};
use tree_sitter::Node;

use super::{
    LanguageExtractor, build_scope, child_field_text, for_each_named_child, node_text, push_symbol,
    push_symbol_with_signature,
};

const ROOT_SCOPE: &str = "module";

/// Extracts symbols and references from Python source files.
pub struct PythonExtractor;

impl LanguageExtractor for PythonExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["py"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_py_node(root, source, file_path, &[], &mut symbols);
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
        walk_py_references(tree.root_node(), source, file_path, symbols, &mut refs);
        refs
    }
}

// ── Symbol extraction ────────────────────────────────────────────────────

fn extract_py_node(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    scope_parts: &[String],
    symbols: &mut Vec<SymbolEntry>,
) {
    let kind = node.kind();
    match kind {
        "function_definition" => {
            if let Some(name) = child_field_text(node, "name", source) {
                let scope = build_scope(scope_parts, ROOT_SCOPE);
                let sym_kind = if scope_parts.is_empty() {
                    SymbolKind::Function
                } else {
                    SymbolKind::Method
                };
                let signature = py_function_signature_bytes(node, source);
                push_symbol_with_signature(
                    symbols, &scope, &name, sym_kind, node, source, file_path, signature,
                );
            }
        }
        "class_definition" => {
            let Some(name) = child_field_text(node, "name", source) else {
                return;
            };
            let scope = build_scope(scope_parts, ROOT_SCOPE);
            let signature = py_class_signature_bytes(node, source);
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
            if let Some(body) = node.child_by_field_name("body") {
                let mut new_scope = scope_parts.to_vec();
                new_scope.push(name);
                for_each_named_child(body, |child| {
                    extract_py_node(child, source, file_path, &new_scope, symbols);
                });
            }
            return;
        }
        "import_statement" | "import_from_statement" => {
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
        _ => {}
    }

    // Recurse into top-level containers
    if matches!(kind, "module" | "block") {
        for_each_named_child(node, |child| {
            extract_py_node(child, source, file_path, scope_parts, symbols);
        });
    }
}

/// Signature bytes for a Python function: everything from `def` up to the
/// body `block`. Includes the parameter list and return annotation.
fn py_function_signature_bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    &source[start..end.min(source.len())]
}

/// Signature bytes for a Python class: the class header and base list,
/// excluding the body.
fn py_class_signature_bytes<'s>(node: Node<'_>, source: &'s [u8]) -> &'s [u8] {
    let start = node.start_byte();
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |b| b.start_byte());
    &source[start..end.min(source.len())]
}

// ── Reference extraction ─────────────────────────────────────────────────

fn walk_py_references(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    refs: &mut Vec<SymbolReference>,
) {
    match node.kind() {
        "call" => {
            // `function` field can be identifier, attribute, or subscript.
            if let Some(function) = node.child_by_field_name("function")
                && let Some((name, scope_hint)) = resolve_py_callee(function, source)
            {
                push_py_reference(
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
            walk_py_import(node, source, file_path, symbols, refs, None);
        }
        "import_from_statement" => {
            // `from X import a, b` — capture each name; module path becomes
            // the scope hint.
            let module_name = node
                .child_by_field_name("module_name")
                .map(|m| node_text(m, source));
            walk_py_import(
                node,
                source,
                file_path,
                symbols,
                refs,
                module_name.as_deref(),
            );
        }
        "class_definition" => {
            // Base classes in the argument list are TypeUse refs.
            if let Some(superclasses) = node.child_by_field_name("superclasses") {
                let mut cursor = superclasses.walk();
                for child in superclasses.named_children(&mut cursor) {
                    if let Some((name, scope_hint)) = resolve_py_type_ref(child, source) {
                        push_py_reference(
                            refs,
                            symbols,
                            child,
                            ReferenceKind::TypeUse,
                            name,
                            scope_hint,
                            file_path,
                        );
                    }
                }
            }
        }
        "type" => {
            // Type annotation nodes (parameter type, return type) appear as
            // `type` nodes in tree-sitter-python. Their child is the actual
            // type expression.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some((name, scope_hint)) = resolve_py_type_ref(child, source) {
                    push_py_reference(
                        refs,
                        symbols,
                        child,
                        ReferenceKind::TypeUse,
                        name,
                        scope_hint,
                        file_path,
                    );
                }
            }
        }
        "decorator" => {
            // @decorator — the decorator target is a reference.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some((name, scope_hint)) = resolve_py_callee(child, source) {
                    push_py_reference(
                        refs,
                        symbols,
                        child,
                        ReferenceKind::Call,
                        name,
                        scope_hint,
                        file_path,
                    );
                    break;
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_py_references(child, source, file_path, symbols, refs);
    }
}

fn walk_py_import(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &[SymbolEntry],
    refs: &mut Vec<SymbolReference>,
    scope_hint: Option<&str>,
) {
    // `import_statement` and `import_from_statement` contain `dotted_name`
    // or `aliased_import` children for each imported symbol.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "dotted_name" => {
                // For `import os.path`, name = "path", scope = "os".
                // For `from X import name`, hint is X; name is the identifier.
                let (name, inner_scope) = split_dotted_name(child, source);
                let final_scope = scope_hint.map(str::to_string).or(inner_scope);
                push_py_reference(
                    refs,
                    symbols,
                    child,
                    ReferenceKind::Import,
                    name,
                    final_scope,
                    file_path,
                );
            }
            "aliased_import" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let (name, inner_scope) = split_dotted_name(name_node, source);
                    let final_scope = scope_hint.map(str::to_string).or(inner_scope);
                    push_py_reference(
                        refs,
                        symbols,
                        name_node,
                        ReferenceKind::Import,
                        name,
                        final_scope,
                        file_path,
                    );
                }
            }
            _ => {}
        }
    }
}

fn split_dotted_name(node: Node<'_>, source: &[u8]) -> (String, Option<String>) {
    let text = node_text(node, source);
    if let Some(last_sep) = text.rfind('.') {
        let (scope, name) = text.split_at(last_sep);
        // Strip the leading "." from `name`.
        let name = name.trim_start_matches('.').to_string();
        (name, Some(scope.to_string()))
    } else {
        (text, None)
    }
}

fn resolve_py_callee(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "identifier" => Some((node_text(node, source), None)),
        "attribute" => {
            let object = node.child_by_field_name("object");
            let attribute = node.child_by_field_name("attribute")?;
            Some((
                node_text(attribute, source),
                object.map(|o| node_text(o, source)),
            ))
        }
        _ => None,
    }
}

fn resolve_py_type_ref(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "identifier" | "type_identifier" => Some((node_text(node, source), None)),
        "attribute" => {
            let object = node.child_by_field_name("object");
            let attribute = node.child_by_field_name("attribute")?;
            Some((
                node_text(attribute, source),
                object.map(|o| node_text(o, source)),
            ))
        }
        "subscript" => {
            // `List[int]` → unwrap to the value expression.
            node.child_by_field_name("value")
                .and_then(|v| resolve_py_type_ref(v, source))
        }
        "generic_type" => node
            .child_by_field_name("name")
            .and_then(|n| resolve_py_type_ref(n, source)),
        _ => None,
    }
}

fn push_py_reference(
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
        .map_or_else(|| py_file_module_id(file_path), |s| s.id.clone());

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

fn py_file_module_id(file_path: &Path) -> SymbolId {
    SymbolId(format!(
        "{ROOT_SCOPE}::__file__::{}::module",
        file_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_py(source: &str) -> (Vec<SymbolEntry>, Vec<SymbolReference>) {
        let mut parser = tree_sitter::Parser::new();
        let extractor = PythonExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let syms = extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.py"));
        let refs =
            extractor.extract_references(&tree, source.as_bytes(), Path::new("test.py"), &syms);
        (syms, refs)
    }

    #[test]
    fn extracts_functions_and_classes() {
        let src = r#"
def greet(name):
    return f"Hello, {name}"

class User:
    def __init__(self, name):
        self.name = name

    def get_name(self):
        return self.name
"#;
        let (symbols, _) = parse_py(src);
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
                .any(|s| s.kind == SymbolKind::Method && s.name == "__init__")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Method && s.name == "get_name")
        );
    }

    #[test]
    fn extracts_imports() {
        let src = r#"
import os
from pathlib import Path
"#;
        let (symbols, _) = parse_py(src);
        let imports: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn signature_hash_stable_for_body_only_change() {
        let (s1, _) = parse_py("def foo(x):\n    return x\n");
        let (s2, _) = parse_py("def foo(x):\n    return x + 1\n");
        assert_eq!(s1[0].signature_hash, s2[0].signature_hash);
        assert_ne!(s1[0].content_hash, s2[0].content_hash);
    }

    #[test]
    fn signature_hash_changes_when_parameters_change() {
        let (s1, _) = parse_py("def foo(x):\n    return x\n");
        let (s2, _) = parse_py("def foo(x, y):\n    return x + y\n");
        assert_ne!(s1[0].signature_hash, s2[0].signature_hash);
    }

    #[test]
    fn signature_hash_changes_when_return_annotation_changes() {
        let (s1, _) = parse_py("def foo(x: int) -> int:\n    return x\n");
        let (s2, _) = parse_py("def foo(x: int) -> str:\n    return str(x)\n");
        assert_ne!(s1[0].signature_hash, s2[0].signature_hash);
    }

    #[test]
    fn captures_function_call() {
        let (_, refs) = parse_py(
            r#"
def login():
    pass

def caller():
    login()
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::Call && r.target_name == "login"),
            "expected a Call ref to login, got: {refs:?}"
        );
    }

    #[test]
    fn captures_method_call_with_scope_hint() {
        let (_, refs) = parse_py(
            r#"
def run():
    auth.login()
"#,
        );
        let login = refs
            .iter()
            .find(|r| r.target_name == "login" && r.kind == ReferenceKind::Call)
            .expect("expected method call to login");
        assert_eq!(login.target_scope_hint.as_deref(), Some("auth"));
    }

    #[test]
    fn captures_from_import_with_module_scope() {
        let (_, refs) = parse_py("from crate.auth import login");
        let login = refs
            .iter()
            .find(|r| r.kind == ReferenceKind::Import && r.target_name == "login")
            .expect("expected login import");
        assert_eq!(login.target_scope_hint.as_deref(), Some("crate.auth"));
    }

    #[test]
    fn captures_class_base() {
        let (_, refs) = parse_py(
            r#"
class Base:
    pass

class Derived(Base):
    pass
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::TypeUse && r.target_name == "Base"),
            "expected TypeUse ref to Base, got: {refs:?}"
        );
    }

    #[test]
    fn captures_decorator() {
        let (_, refs) = parse_py(
            r#"
def cache(f):
    return f

@cache
def foo():
    pass
"#,
        );
        assert!(
            refs.iter()
                .any(|r| r.kind == ReferenceKind::Call && r.target_name == "cache"),
            "expected decorator to produce Call ref to cache, got: {refs:?}"
        );
    }

    #[test]
    fn self_recursion_does_not_emit_self_edge() {
        let (_, refs) = parse_py(
            r#"
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)
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
