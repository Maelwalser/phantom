//! Per-language symbol extraction configurations.
//!
//! Each language implements [`LanguageExtractor`] to map tree-sitter CST nodes
//! to [`SymbolEntry`] values using Weave-style entity matching by composite key
//! `(name, kind, scope)`.

pub mod bash;
pub mod css;
pub mod dockerfile;
pub mod go;
pub mod hcl;
pub mod json;
pub mod makefile;
pub mod python;
pub mod rust;
pub mod toml;
pub mod typescript;
pub mod yaml;

use std::path::Path;

use phantom_core::id::{ContentHash, SymbolId};
use phantom_core::symbol::{SymbolEntry, SymbolKind};
use tree_sitter::Node;

/// Trait for extracting symbols from a tree-sitter parse tree.
///
/// Each supported language provides an implementation that walks the CST and
/// produces [`SymbolEntry`] values for top-level declarations.
pub trait LanguageExtractor: Send + Sync {
    /// The tree-sitter language grammar.
    fn language(&self) -> tree_sitter::Language;

    /// File extensions handled by this extractor (without the leading dot).
    fn extensions(&self) -> &[&str];

    /// Extract symbols from a parsed tree-sitter tree.
    fn extract_symbols(
        &self,
        tree: &tree_sitter::Tree,
        source: &[u8],
        file_path: &Path,
    ) -> Vec<SymbolEntry>;

    /// Exact filenames this extractor handles (e.g., `"Dockerfile"`, `"Makefile"`).
    ///
    /// Used for files that lack a meaningful extension. Checked before extension
    /// matching. Default: empty (extension-based matching only).
    fn filenames(&self) -> &[&str] {
        &[]
    }
}

/// Return all built-in language extractors.
///
/// Centralizes registration so adding a new language only requires adding
/// the module above and a line here — `Parser::new()` just iterates this list.
pub fn all_extractors() -> Vec<Box<dyn LanguageExtractor>> {
    vec![
        // Programming languages
        Box::new(rust::RustExtractor),
        Box::new(typescript::TypeScriptExtractor::new()),
        Box::new(typescript::TypeScriptExtractor::tsx()),
        Box::new(python::PythonExtractor),
        Box::new(go::GoExtractor),
        // Config & infrastructure files
        Box::new(yaml::YamlExtractor),
        Box::new(toml::TomlExtractor),
        Box::new(json::JsonExtractor),
        Box::new(dockerfile::DockerfileExtractor),
        Box::new(bash::BashExtractor),
        Box::new(hcl::HclExtractor),
        Box::new(css::CssExtractor),
        Box::new(makefile::MakefileExtractor),
    ]
}

// ── Shared helpers used by all language extractors ──────────────────────

/// Extract text of a child field from a tree-sitter node.
pub(crate) fn child_field_text(node: Node<'_>, field: &str, source: &[u8]) -> Option<String> {
    let child = node.child_by_field_name(field)?;
    child.utf8_text(source).ok().map(std::string::ToString::to_string)
}

/// Extract the full text of a tree-sitter node.
pub(crate) fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").to_string()
}

/// Build a scope string from parts with a language-specific root prefix.
pub(crate) fn build_scope(parts: &[String], root: &str) -> String {
    if parts.is_empty() {
        root.to_string()
    } else {
        format!("{root}::{}", parts.join("::"))
    }
}

/// Create a [`SymbolEntry`] and push it onto the symbol list.
pub(crate) fn push_symbol(
    symbols: &mut Vec<SymbolEntry>,
    scope: &str,
    name: &str,
    kind: SymbolKind,
    node: Node<'_>,
    source: &[u8],
    file_path: &Path,
) {
    let kind_str = format!("{kind:?}").to_lowercase();
    let id = SymbolId(format!("{scope}::{name}::{kind_str}"));
    let content = &source[node.start_byte()..node.end_byte()];
    let content_hash = ContentHash::from_bytes(content);

    symbols.push(SymbolEntry {
        id,
        kind,
        name: name.to_string(),
        scope: scope.to_string(),
        file: file_path.to_path_buf(),
        byte_range: node.start_byte()..node.end_byte(),
        content_hash,
    });
}
