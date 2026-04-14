use super::*;
use std::fs;

#[test]
fn build_from_directory_indexes_rust_files() {
    let dir = tempfile::tempdir().unwrap();
    let src_dir = dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    fs::write(src_dir.join("main.rs"), "fn main() {}\nfn helper() {}").unwrap();
    fs::write(src_dir.join("lib.rs"), "pub fn public_fn() {}").unwrap();

    let parser = Parser::new();
    let index =
        InMemorySymbolIndex::build_from_directory(dir.path(), &parser, GitOid::zero()).unwrap();

    assert!(index.len() >= 3);
}

#[test]
fn update_and_remove_file() {
    let mut index = InMemorySymbolIndex::new(GitOid::zero());
    let parser = Parser::new();
    let path = Path::new("test.rs");
    let content = b"fn foo() {}\nfn bar() {}";
    let symbols = parser.parse_file(path, content).unwrap();
    assert_eq!(symbols.len(), 2);

    index.update_file(path, symbols);
    assert_eq!(index.symbols_in_file(path).len(), 2);

    // Update with new content (only one function)
    let new_symbols = parser.parse_file(path, b"fn foo() {}").unwrap();
    index.update_file(path, new_symbols);
    assert_eq!(index.symbols_in_file(path).len(), 1);

    // Remove file
    index.remove_file(path);
    assert!(index.symbols_in_file(path).is_empty());
    assert!(index.is_empty());
}
