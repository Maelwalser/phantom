//! Cross-domain signature extraction for plan domain instructions.
//!
//! Extracts compact symbol signatures from files owned by other parallel
//! domains and formats them as a markdown section for injection into the
//! agent's system prompt. This gives agents immediate structural awareness
//! of the parallel work environment without consuming tool-call tokens.

mod strip;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;

use phantom_core::plan::{Plan, PlanDomain};
use phantom_core::symbol::SymbolKind;
use phantom_semantic::Parser;
use tracing::debug;

use strip::strip_body;

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

        let Ok(source) = std::str::from_utf8(&content) else {
            continue;
        };

        let lang_tag = crate::context_file::lang_from_path(rel_path);
        let ext = rel_path.extension().and_then(|e| e.to_str()).unwrap_or("");

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

/// Check if a file path looks like a test file.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_test_file(path: &Path) -> bool {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
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
