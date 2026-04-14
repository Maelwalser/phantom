use super::*;

#[test]
fn lang_from_path_maps_correctly() {
    assert_eq!(lang_from_path(Path::new("foo.rs")), "rust");
    assert_eq!(lang_from_path(Path::new("bar.ts")), "typescript");
    assert_eq!(lang_from_path(Path::new("baz.py")), "python");
    assert_eq!(lang_from_path(Path::new("qux.go")), "go");
    assert_eq!(lang_from_path(Path::new("unknown.txt")), "");
}
