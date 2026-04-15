//! Makefile symbol extraction via `tree-sitter-make`.
//!
//! Targets become `Function` symbols and variable assignments become `Variable`
//! symbols, enabling semantic merge of independent Makefile sections.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, node_text, push_symbol};

/// Extracts symbols from Makefiles.
pub struct MakefileExtractor;

impl LanguageExtractor for MakefileExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_make::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["mk"]
    }

    fn filenames(&self) -> &[&str] {
        &["Makefile", "makefile", "GNUmakefile"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_make_top_level(root, source, file_path, &mut symbols);
        symbols
    }
}

fn extract_make_top_level(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "rule" => {
                let target = extract_rule_target(child, source);
                if !target.is_empty() {
                    push_symbol(
                        symbols,
                        "makefile",
                        &target,
                        SymbolKind::Function,
                        child,
                        source,
                        file_path,
                    );
                }
            }
            "variable_assignment" => {
                let name = extract_make_var_name(child, source);
                if !name.is_empty() {
                    push_symbol(
                        symbols,
                        "makefile",
                        &name,
                        SymbolKind::Variable,
                        child,
                        source,
                        file_path,
                    );
                }
            }
            // Handle .PHONY, include, etc. as directives.
            "include_directive" | "define_directive" | "VPATH_assignment" => {
                let text = node_text(child, source)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                push_symbol(
                    symbols,
                    "makefile",
                    &text,
                    SymbolKind::Directive,
                    child,
                    source,
                    file_path,
                );
            }
            // Recurse into the top-level makefile node.
            "makefile" => {
                extract_make_top_level(child, source, file_path, symbols);
            }
            _ => {}
        }
    }
}

/// Extract the target name(s) from a rule node.
fn extract_rule_target(rule_node: Node<'_>, source: &[u8]) -> String {
    let mut cursor = rule_node.walk();
    for child in rule_node.named_children(&mut cursor) {
        // The targets might be named "targets" or individual "word" nodes.
        if child.kind() == "targets" || child.kind() == "word" || child.kind() == "list" {
            return node_text(child, source).trim().to_string();
        }
    }
    // Fallback: text before the first ':'.
    let text = node_text(rule_node, source);
    text.split(':').next().unwrap_or("").trim().to_string()
}

/// Extract variable name from a variable_assignment node.
fn extract_make_var_name(node: Node<'_>, source: &[u8]) -> String {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "word" || child.kind() == "variable_name" {
            return node_text(child, source).trim().to_string();
        }
    }
    // Fallback: text before '=' or ':='.
    let text = node_text(node, source);
    text.split(['=', ':'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
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
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Variable && s.name == "CC")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Variable && s.name == "CFLAGS")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Function && s.name.contains("all"))
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == SymbolKind::Function && s.name.contains("clean"))
        );
    }

    #[test]
    fn handles_phony() {
        let src = ".PHONY: all clean\n\nall:\n\techo build\n";
        let symbols = parse_makefile(src);
        // .PHONY is a target rule.
        assert!(!symbols.is_empty());
    }
}
