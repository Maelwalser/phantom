use std::path::Path;

use phantom_core::symbol::SymbolKind;

use super::strip::{strip_brace_body, strip_python_body};
use super::{extract_signature_text, is_test_file};

#[test]
fn strip_brace_body_rust_function() {
    let src = "pub fn validate(token: &str) -> Result<Claims, Error> {\n    todo!()\n}";
    let sig = strip_brace_body(src);
    assert_eq!(
        sig,
        "pub fn validate(token: &str) -> Result<Claims, Error> { ... }"
    );
}

#[test]
fn strip_brace_body_with_generics() {
    let src = "fn process<T: Into<String>>(items: Vec<T>) -> HashMap<String, T> {\n    todo!()\n}";
    let sig = strip_brace_body(src);
    assert_eq!(
        sig,
        "fn process<T: Into<String>>(items: Vec<T>) -> HashMap<String, T> { ... }"
    );
}

#[test]
fn strip_brace_body_typescript() {
    let src = "function greet(name: string): string {\n    return `Hello, ${name}`;\n}";
    let sig = strip_brace_body(src);
    assert_eq!(sig, "function greet(name: string): string { ... }");
}

#[test]
fn strip_brace_body_go() {
    let src = "func (s *Server) Start() error {\n\treturn nil\n}";
    let sig = strip_brace_body(src);
    assert_eq!(sig, "func (s *Server) Start() error { ... }");
}

#[test]
fn strip_brace_body_no_body() {
    let src = "fn abstract_method(&self) -> bool;";
    let sig = strip_brace_body(src);
    assert_eq!(sig, "fn abstract_method(&self) -> bool;");
}

#[test]
fn strip_python_body_simple() {
    let src = "def greet(name: str) -> str:\n    return f\"Hello, {name}\"";
    let sig = strip_python_body(src);
    assert_eq!(sig, "def greet(name: str) -> str:");
}

#[test]
fn strip_python_body_no_return_type() {
    let src = "def __init__(self, name):\n    self.name = name";
    let sig = strip_python_body(src);
    assert_eq!(sig, "def __init__(self, name):");
}

#[test]
fn struct_kept_as_is() {
    let src = "pub struct Config {\n    pub host: String,\n    pub port: u16,\n}";
    let sig = extract_signature_text(src, SymbolKind::Struct, "rs");
    assert_eq!(sig, src);
}

#[test]
fn impl_returns_empty() {
    let sig = extract_signature_text("impl Foo { fn bar() {} }", SymbolKind::Impl, "rs");
    assert!(sig.is_empty());
}

#[test]
fn import_returns_empty() {
    let sig = extract_signature_text("use std::collections::HashMap;", SymbolKind::Import, "rs");
    assert!(sig.is_empty());
}

#[test]
fn is_test_file_detects_patterns() {
    assert!(is_test_file(Path::new("src/auth_test.rs")));
    assert!(is_test_file(Path::new("tests/test_utils.py")));
    assert!(is_test_file(Path::new("src/auth.spec.ts")));
    assert!(!is_test_file(Path::new("src/auth.rs")));
    assert!(!is_test_file(Path::new("src/testing.rs")));
}
