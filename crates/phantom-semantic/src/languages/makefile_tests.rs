use super::*;

fn parse_makefile(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = MakefileExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("Makefile"))
}

#[test]
fn extracts_targets_and_variables() {
    let src = "CC = gcc\nCFLAGS = -Wall\n\nall: main.o\n\techo done\n\nclean:\n\trm -f *.o\n";
    let symbols = parse_makefile(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Variable && s.name == "CC"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Variable && s.name == "CFLAGS"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function && s.name.contains("all")));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function && s.name.contains("clean")));
}

#[test]
fn handles_phony() {
    let src = ".PHONY: all clean\n\nall:\n\techo build\n";
    let symbols = parse_makefile(src);
    // .PHONY is a target rule.
    assert!(!symbols.is_empty());
}
