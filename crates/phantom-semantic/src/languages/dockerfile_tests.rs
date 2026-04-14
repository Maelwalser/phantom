use super::*;
use std::path::Path;

fn parse_dockerfile(source: &str) -> Vec<SymbolEntry> {
    // Dockerfile extractor ignores the tree-sitter tree and parses raw text.
    let mut parser = tree_sitter::Parser::new();
    let extractor = DockerfileExtractor;
    parser.set_language(&extractor.language()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    extractor.extract_symbols(&tree, source.as_bytes(), Path::new("Dockerfile"))
}

#[test]
fn extracts_single_stage() {
    let src = "FROM rust:1.85\nRUN cargo build\nCOPY . /app\n";
    let symbols = parse_dockerfile(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "rust:1.85"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Directive && s.name.starts_with("RUN")));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Directive && s.name.starts_with("COPY")));
}

#[test]
fn extracts_multi_stage_build() {
    let src = r#"FROM rust:1.85 AS builder
RUN cargo build --release

FROM debian:bookworm-slim AS runtime
COPY --from=builder /app/target/release/app /usr/local/bin/
CMD ["/usr/local/bin/app"]
"#;
    let symbols = parse_dockerfile(src);
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "builder"));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name == "runtime"));
    // Each stage should have its own directives.
    let builder_directives: Vec<_> = symbols.iter()
        .filter(|s| s.scope == "builder" && s.kind == SymbolKind::Directive)
        .collect();
    assert!(!builder_directives.is_empty());
}

#[test]
fn skips_comments() {
    let src = "# This is a comment\nFROM alpine\nRUN echo hello\n";
    let symbols = parse_dockerfile(src);
    // Should not create symbols for comments.
    assert!(!symbols.iter().any(|s| s.name.contains("comment")));
    assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section));
}
