//! CSS symbol extraction via `tree-sitter-css`.
//!
//! Rule sets become `Section` symbols (keyed by selector) and @-rules become
//! `Directive` symbols, enabling semantic merge of independent CSS rules.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{
    LanguageExtractor, first_child_text_of_kind, for_each_named_child, node_text, push_symbol,
};

const ROOT_SCOPE: &str = "stylesheet";

/// Extracts symbols from CSS files.
pub struct CssExtractor;

impl LanguageExtractor for CssExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_css::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["css"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_css_top_level(root, source, file_path, &mut symbols);
        symbols
    }
}

fn extract_css_top_level(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    for_each_named_child(node, |child| match child.kind() {
        "rule_set" => {
            let selector = extract_selector(child, source);
            if !selector.is_empty() {
                push_symbol(
                    symbols,
                    ROOT_SCOPE,
                    &selector,
                    SymbolKind::Section,
                    child,
                    source,
                    file_path,
                );
            }
        }
        "import_statement" | "charset_statement" | "namespace_statement" => {
            let text = node_text(child, source).trim().to_string();
            let name = text
                .lines()
                .next()
                .unwrap_or(&text)
                .trim_end_matches(';')
                .trim()
                .to_string();
            push_symbol(
                symbols,
                ROOT_SCOPE,
                &name,
                SymbolKind::Directive,
                child,
                source,
                file_path,
            );
        }
        "media_statement" | "supports_statement" | "keyframes_statement" | "at_rule" => {
            let name = extract_at_rule_name(child, source);
            push_symbol(
                symbols,
                ROOT_SCOPE,
                &name,
                SymbolKind::Section,
                child,
                source,
                file_path,
            );
        }
        // Recurse into stylesheet root.
        "stylesheet" => extract_css_top_level(child, source, file_path, symbols),
        _ => {}
    });
}

/// Extract the selector text from a rule_set node.
fn extract_selector(rule_node: Node<'_>, source: &[u8]) -> String {
    if let Some(text) = first_child_text_of_kind(rule_node, source, &["selectors"]) {
        return text.trim().to_string();
    }
    // Fallback: take text before the first '{'.
    let text = node_text(rule_node, source);
    text.split('{').next().unwrap_or("").trim().to_string()
}

/// Extract a name for an @-rule (e.g., `@media (min-width: 768px)`).
fn extract_at_rule_name(node: Node<'_>, source: &[u8]) -> String {
    let text = node_text(node, source);
    // Take everything before the opening '{' as the identifier.
    text.split('{').next().unwrap_or(&text).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_css(source: &str) -> Vec<SymbolEntry> {
        let mut parser = tree_sitter::Parser::new();
        let extractor = CssExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("styles.css"))
    }

    #[test]
    fn extracts_rule_sets() {
        let src = r#"
    body {
        margin: 0;
        padding: 0;
    }

    .container {
        max-width: 1200px;
    }

    #header {
        background: blue;
    }
    "#;
        let symbols = parse_css(src);
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == "body")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == ".container")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == "#header")
        );
    }

    #[test]
    fn extracts_media_queries() {
        let src = r#"
    @media (min-width: 768px) {
        .container { max-width: 720px; }
    }
    "#;
        let symbols = parse_css(src);
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name.contains("@media"))
        );
    }

    #[test]
    fn extracts_import_directives() {
        let src = r#"
    @import url("reset.css");
    body { color: black; }
    "#;
        let symbols = parse_css(src);
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Directive));
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Section && s.name == "body")
        );
    }
}
