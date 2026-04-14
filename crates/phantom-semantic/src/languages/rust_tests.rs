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
