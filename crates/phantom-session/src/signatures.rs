//! Cross-domain signature extraction for plan domain instructions.
//!
//! Extracts compact symbol signatures from files owned by other parallel
//! domains and formats them as a markdown section for injection into the
//! agent's system prompt. This gives agents immediate structural awareness
//! of the parallel work environment without consuming tool-call tokens.

use std::collections::BTreeMap;
use std::path::Path;

use phantom_core::plan::{Plan, PlanDomain};
use phantom_core::symbol::SymbolKind;
use phantom_semantic::Parser;
use tracing::debug;

/// Maximum file size to process (256 KB). Larger files are likely generated.
const MAX_FILE_SIZE: usize = 256 * 1024;

/// Default byte budget for the entire signatures section.
const DEFAULT_BUDGET: usize = 4096;

/// Extract cross-domain signatures and format as a markdown section.
///
/// Returns an empty string if no signatures could be extracted (all files
/// unsupported, missing, or empty).
pub fn extract_cross_domain_signatures(
    repo_root: &Path,
    current_domain: &PlanDomain,
    plan: &Plan,
) -> String {
    let parser = Parser::new();
    let mut budget = DEFAULT_BUDGET;

    // Collect files from other domains, grouped by (file_path, domain_name).
    // Use BTreeMap for deterministic ordering.
    let mut file_to_domain: BTreeMap<&Path, &str> = BTreeMap::new();
    for domain in &plan.domains {
        if domain.name == current_domain.name {
            continue;
        }
        for file in &domain.files_to_modify {
            if !is_test_file(file) {
                file_to_domain.insert(file.as_path(), &domain.name);
            }
        }
    }

    if file_to_domain.is_empty() {
        return String::new();
    }

    let mut sections: Vec<String> = Vec::new();

    for (rel_path, domain_name) in &file_to_domain {
        let abs_path = repo_root.join(rel_path);

        let content = match std::fs::read(&abs_path) {
            Ok(c) if c.len() <= MAX_FILE_SIZE => c,
            Ok(_) => {
                debug!(path = %abs_path.display(), "skipping oversized file");
                continue;
            }
            Err(_) => {
                debug!(path = %abs_path.display(), "skipping unreadable file");
                continue;
            }
        };

        let symbols = match parser.parse_file(rel_path, &content) {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => continue,
            Err(_) => {
                debug!(path = %rel_path.display(), "skipping unparseable file");
                continue;
            }
        };

        let source = match std::str::from_utf8(&content) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let ext = rel_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let lang_tag = ext_to_lang_tag(ext);

        // Sort symbols: type definitions first, then functions/methods.
        let mut sorted_symbols = symbols;
        sorted_symbols.sort_by_key(|s| match s.kind {
            SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::Trait
            | SymbolKind::Interface
            | SymbolKind::Class => 0,
            SymbolKind::Const | SymbolKind::TypeAlias => 1,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Test => 2,
            _ => 3,
        });

        let mut sigs: Vec<String> = Vec::new();
        for sym in &sorted_symbols {
            // Skip imports, impl blocks, and modules (children are extracted individually).
            if matches!(
                sym.kind,
                SymbolKind::Import | SymbolKind::Impl | SymbolKind::Module
            ) {
                continue;
            }

            let sym_text = &source[sym.byte_range.clone()];
            let sig = extract_signature_text(sym_text, sym.kind, ext);
            if sig.is_empty() {
                continue;
            }
            sigs.push(sig);
        }

        if sigs.is_empty() {
            continue;
        }

        let sig_block = sigs.join("\n");
        let section = format!(
            "### `{}` (domain: {})\n```{lang_tag}\n{sig_block}\n```\n",
            rel_path.display(),
            domain_name,
        );

        if section.len() > budget {
            // If even the first section doesn't fit, try to include a truncated version.
            if sections.is_empty() && budget > 100 {
                let truncated = truncate_to_budget(&section, budget);
                sections.push(truncated);
            }
            break;
        }

        budget -= section.len();
        sections.push(section);
    }

    if sections.is_empty() {
        return String::new();
    }

    let mut result = String::from(
        "## Cross-Domain API Surface\n\n\
         The following signatures are from files owned by other parallel domains.\n\
         Use these to code defensively against their APIs without needing to read the files.\n\n",
    );
    for section in sections {
        result.push_str(&section);
        result.push('\n');
    }
    result
}

/// Extract a compact signature from a symbol's source text.
///
/// For functions/methods, strips the body. For type definitions, keeps the
/// full text since field definitions are the API surface.
fn extract_signature_text(source: &str, kind: SymbolKind, file_ext: &str) -> String {
    match kind {
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Test => {
            strip_body(source, file_ext)
        }
        SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Trait
        | SymbolKind::Interface
        | SymbolKind::Class
        | SymbolKind::Const
        | SymbolKind::TypeAlias => source.to_string(),
        // Impl, Module, Import are filtered out before reaching here.
        _ => String::new(),
    }
}

/// Strip the body from a function/method, keeping only the signature.
fn strip_body(source: &str, file_ext: &str) -> String {
    if file_ext == "py" {
        strip_python_body(source)
    } else {
        strip_brace_body(source)
    }
}

/// Strip a brace-delimited body (Rust, TypeScript, Go).
///
/// Finds the first `{` at bracket-depth 0, which starts the function body,
/// and replaces everything from there with `{ ... }`.
fn strip_brace_body(source: &str) -> String {
    let mut depth: i32 = 0;
    for (i, ch) in source.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            '{' if depth == 0 => {
                let sig = source[..i].trim_end();
                return format!("{sig} {{ ... }}");
            }
            _ => {}
        }
    }
    // No body found — return as-is (e.g. abstract method declaration).
    source.to_string()
}

/// Strip a Python function body (everything after the signature colon).
fn strip_python_body(source: &str) -> String {
    let mut depth: i32 = 0;
    let mut past_params = false;
    for (i, ch) in source.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth = (depth - 1).max(0);
                if depth == 0 {
                    past_params = true;
                }
            }
            ':' if depth == 0 && past_params => {
                return source[..=i].trim_end().to_string();
            }
            _ => {}
        }
    }
    // Fallback: take first line.
    source.lines().next().unwrap_or(source).to_string()
}

/// Map file extensions to markdown code block language tags.
fn ext_to_lang_tag(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        _ => "",
    }
}

/// Check if a file path looks like a test file.
fn is_test_file(path: &Path) -> bool {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    name.ends_with("_test")
        || name.ends_with("_spec")
        || name.starts_with("test_")
        || name.ends_with(".test")
        || name.ends_with(".spec")
}

/// Truncate a section to fit within a byte budget, preserving valid markdown.
fn truncate_to_budget(section: &str, budget: usize) -> String {
    // Find a line boundary within budget and close the code block.
    let mut last_newline = 0;
    for (i, ch) in section.char_indices() {
        if i >= budget.saturating_sub(20) {
            break;
        }
        if ch == '\n' {
            last_newline = i;
        }
    }
    if last_newline > 0 {
        format!("{}\n```\n", &section[..last_newline])
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_brace_body_rust_function() {
        let src = "pub fn validate(token: &str) -> Result<Claims, Error> {\n    todo!()\n}";
        let sig = strip_brace_body(src);
        assert_eq!(sig, "pub fn validate(token: &str) -> Result<Claims, Error> { ... }");
    }

    #[test]
    fn strip_brace_body_with_generics() {
        let src = "fn process<T: Into<String>>(items: Vec<T>) -> HashMap<String, T> {\n    todo!()\n}";
        let sig = strip_brace_body(src);
        assert_eq!(
            sig,
            "fn process<T: Into<String>>(items: Vec<T>) -> HashMap<String, T> { ... }"
        );
    }

    #[test]
    fn strip_brace_body_typescript() {
        let src = "function greet(name: string): string {\n    return `Hello, ${name}`;\n}";
        let sig = strip_brace_body(src);
        assert_eq!(sig, "function greet(name: string): string { ... }");
    }

    #[test]
    fn strip_brace_body_go() {
        let src = "func (s *Server) Start() error {\n\treturn nil\n}";
        let sig = strip_brace_body(src);
        assert_eq!(sig, "func (s *Server) Start() error { ... }");
    }

    #[test]
    fn strip_brace_body_no_body() {
        let src = "fn abstract_method(&self) -> bool;";
        let sig = strip_brace_body(src);
        assert_eq!(sig, "fn abstract_method(&self) -> bool;");
    }

    #[test]
    fn strip_python_body_simple() {
        let src = "def greet(name: str) -> str:\n    return f\"Hello, {name}\"";
        let sig = strip_python_body(src);
        assert_eq!(sig, "def greet(name: str) -> str:");
    }

    #[test]
    fn strip_python_body_no_return_type() {
        let src = "def __init__(self, name):\n    self.name = name";
        let sig = strip_python_body(src);
        assert_eq!(sig, "def __init__(self, name):");
    }

    #[test]
    fn struct_kept_as_is() {
        let src = "pub struct Config {\n    pub host: String,\n    pub port: u16,\n}";
        let sig = extract_signature_text(src, SymbolKind::Struct, "rs");
        assert_eq!(sig, src);
    }

    #[test]
    fn impl_returns_empty() {
        let sig = extract_signature_text("impl Foo { fn bar() {} }", SymbolKind::Impl, "rs");
        assert!(sig.is_empty());
    }

    #[test]
    fn import_returns_empty() {
        let sig =
            extract_signature_text("use std::collections::HashMap;", SymbolKind::Import, "rs");
        assert!(sig.is_empty());
    }

    #[test]
    fn is_test_file_detects_patterns() {
        assert!(is_test_file(Path::new("src/auth_test.rs")));
        assert!(is_test_file(Path::new("tests/test_utils.py")));
        assert!(is_test_file(Path::new("src/auth.spec.ts")));
        assert!(!is_test_file(Path::new("src/auth.rs")));
        assert!(!is_test_file(Path::new("src/testing.rs")));
    }

    #[test]
    fn ext_to_lang_tag_maps_correctly() {
        assert_eq!(ext_to_lang_tag("rs"), "rust");
        assert_eq!(ext_to_lang_tag("ts"), "typescript");
        assert_eq!(ext_to_lang_tag("py"), "python");
        assert_eq!(ext_to_lang_tag("go"), "go");
        assert_eq!(ext_to_lang_tag("txt"), "");
    }
}
