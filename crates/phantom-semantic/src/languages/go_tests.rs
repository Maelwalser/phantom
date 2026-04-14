use super::*;

fn parse_go(source: &str) -> Vec<SymbolEntry> {
    let mut parser = tree_sitter::Parser::new();
    let extractor = GoExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("test.go"))
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
    let symbols = parse_go(src);
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
    let symbols = parse_go(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Import));
    assert!(
        symbols
            .iter()
            .any(|s| s.kind == SymbolKind::Interface && s.name == "Handler")
    );
}
