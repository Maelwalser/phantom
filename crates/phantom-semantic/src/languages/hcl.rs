//! HCL/Terraform symbol extraction via `tree-sitter-hcl`.
//!
//! Resource, data, variable, output, and other blocks become `Section` symbols,
//! enabling semantic merge of independent infrastructure definitions.

use std::path::Path;

use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

use super::{LanguageExtractor, node_text, push_symbol};

/// Extracts symbols from HCL/Terraform files.
pub struct HclExtractor;

impl LanguageExtractor for HclExtractor {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_hcl::LANGUAGE.into()
    }

    fn extensions(&self) -> &[&str] {
        &["tf", "hcl"]
    }

    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_hcl_top_level(root, source, file_path, &mut symbols);
        symbols
    }
}

fn extract_hcl_top_level(
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
    symbols: &mut Vec<SymbolEntry>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "block" => {
                let name = extract_block_identifier(child, source);
                if !name.is_empty() {
                    push_symbol(
                        symbols,
                        "root",
                        &name,
                        SymbolKind::Section,
                        child,
                        source,
                        file_path,
                    );
                }
            }
            "attribute" => {
                if let Some(key_node) = child.child_by_field_name("key") {
                    let key = node_text(key_node, source).trim().to_string();
                    if !key.is_empty() {
                        push_symbol(
                            symbols,
                            "root",
                            &key,
                            SymbolKind::Section,
                            child,
                            source,
                            file_path,
                        );
                    }
                }
            }
            // Recurse into config_file or body nodes.
            "config_file" | "body" => {
                extract_hcl_top_level(child, source, file_path, symbols);
            }
            _ => {}
        }
    }
}

/// Build a block identifier from block type + labels.
/// E.g., `resource "aws_instance" "web"` → `"resource.aws_instance.web"`.
fn extract_block_identifier(block_node: Node<'_>, source: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut cursor = block_node.walk();
    for child in block_node.named_children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                parts.push(node_text(child, source).trim().to_string());
            }
            "string_lit" => {
                let text = node_text(child, source)
                    .trim()
                    .trim_matches('"')
                    .to_string();
                parts.push(text);
            }
            "body" | "block" => break, // Stop at the body
            _ => {}
        }
    }
    parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_hcl(source: &str) -> Vec<SymbolEntry> {
        let mut parser = tree_sitter::Parser::new();
        let extractor = HclExtractor;
        parser.set_language(&extractor.language()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        extractor.extract_symbols(&tree, source.as_bytes(), Path::new("main.tf"))
    }

    #[test]
    fn extracts_terraform_resources() {
        let src = r#"
    resource "aws_instance" "web" {
      ami           = "ami-12345"
      instance_type = "t2.micro"
    }

    resource "aws_s3_bucket" "data" {
      bucket = "my-data-bucket"
    }
    "#;
        let symbols = parse_hcl(src);
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name.contains("aws_instance")));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Section && s.name.contains("aws_s3_bucket")));
    }

    #[test]
    fn extracts_variables_and_outputs() {
        let src = r#"
    variable "region" {
      default = "us-east-1"
    }

    output "instance_ip" {
      value = aws_instance.web.public_ip
    }
    "#;
        let symbols = parse_hcl(src);
        assert!(symbols.iter().any(|s| s.name.contains("variable") && s.name.contains("region")));
        assert!(symbols.iter().any(|s| s.name.contains("output") && s.name.contains("instance_ip")));
    }

    #[test]
    fn extracts_provider_block() {
        let src = r#"
    provider "aws" {
      region = "us-east-1"
    }
    "#;
        let symbols = parse_hcl(src);
        assert!(symbols.iter().any(|s| s.name.contains("provider") && s.name.contains("aws")));
    }
}
