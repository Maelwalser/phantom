//! Per-language symbol extraction configurations.
//!
//! Each language implements [`LanguageExtractor`] to map tree-sitter CST nodes
//! to [`SymbolEntry`] values using Weave-style entity matching by composite key
//! `(name, kind, scope)`.

pub mod go;
pub mod python;
pub mod rust;
pub mod typescript;

use std::path::Path;

use phantom_core::SymbolEntry;

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
}
