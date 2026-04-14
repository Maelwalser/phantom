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
