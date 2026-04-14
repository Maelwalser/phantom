//! Tree-sitter parsing and language-aware symbol extraction.
//!
//! The [`Parser`] holds registered language extractors and delegates symbol
//! extraction to the appropriate one based on file extension.

use std::collections::HashMap;
use std::path::Path;

use phantom_core::SymbolEntry;

use crate::error::SemanticError;
use crate::languages::LanguageExtractor;
use crate::languages::bash::BashExtractor;
use crate::languages::css::CssExtractor;
use crate::languages::dockerfile::DockerfileExtractor;
use crate::languages::go::GoExtractor;
use crate::languages::hcl::HclExtractor;
use crate::languages::json::JsonExtractor;
use crate::languages::makefile::MakefileExtractor;
use crate::languages::python::PythonExtractor;
use crate::languages::rust::RustExtractor;
use crate::languages::toml::TomlExtractor;
use crate::languages::typescript::TypeScriptExtractor;
use crate::languages::yaml::YamlExtractor;

/// Multi-language parser that routes files to the right tree-sitter grammar.
pub struct Parser {
    /// Maps file extension to extractor index.
    ext_to_index: HashMap<String, usize>,
    /// Maps exact filename to extractor index (for Dockerfile, Makefile, etc.).
    name_to_index: HashMap<String, usize>,
    /// Registered extractors.
    extractors: Vec<Box<dyn LanguageExtractor>>,
}

impl Parser {
    /// Create a new parser with all built-in language extractors registered.
    #[must_use]
    pub fn new() -> Self {
        let mut parser = Self {
            ext_to_index: HashMap::new(),
            name_to_index: HashMap::new(),
            extractors: Vec::new(),
        };
        // Programming languages
        parser.register(Box::new(RustExtractor));
        parser.register(Box::new(TypeScriptExtractor::new()));
        parser.register(Box::new(TypeScriptExtractor::tsx()));
        parser.register(Box::new(PythonExtractor));
        parser.register(Box::new(GoExtractor));
        // Config & infrastructure files
        parser.register(Box::new(YamlExtractor));
        parser.register(Box::new(TomlExtractor));
        parser.register(Box::new(JsonExtractor));
        parser.register(Box::new(DockerfileExtractor));
        parser.register(Box::new(BashExtractor));
        parser.register(Box::new(HclExtractor));
        parser.register(Box::new(CssExtractor));
        parser.register(Box::new(MakefileExtractor));
        parser
    }

    /// Create an empty parser with no language extractors registered.
    ///
    /// Use [`register`](Self::register) to add extractors selectively.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            ext_to_index: HashMap::new(),
            name_to_index: HashMap::new(),
            extractors: Vec::new(),
        }
    }

    /// Register a language extractor, mapping each of its extensions and filenames.
    pub fn register(&mut self, extractor: Box<dyn LanguageExtractor>) {
        let idx = self.extractors.len();
        for ext in extractor.extensions() {
            self.ext_to_index.insert(ext.to_string(), idx);
        }
        for name in extractor.filenames() {
            self.name_to_index.insert(name.to_string(), idx);
        }
        self.extractors.push(extractor);
    }

    /// Resolve the extractor index for a given path, checking filename first then extension.
    fn resolve_index(&self, path: &Path) -> Option<usize> {
        // Check exact filename first (e.g., "Dockerfile", "Makefile").
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && let Some(&idx) = self.name_to_index.get(name)
        {
            return Some(idx);
        }
        // Fall back to extension-based lookup.
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(|ext| self.ext_to_index.get(ext).copied())
    }

    /// Parse a file and extract its symbols.
    pub fn parse_file(
        &self,
        path: &Path,
        content: &[u8],
    ) -> Result<Vec<SymbolEntry>, SemanticError> {
        let idx = self.resolve_index(path).ok_or_else(|| {
            SemanticError::UnsupportedLanguage {
                path: path.to_path_buf(),
            }
        })?;

        let extractor = &self.extractors[idx];
        let language = extractor.language();

        let mut ts_parser = tree_sitter::Parser::new();
        ts_parser
            .set_language(&language)
            .map_err(|e| SemanticError::ParseError {
                path: path.to_path_buf(),
                detail: format!("failed to set language: {e}"),
            })?;

        let tree = ts_parser
            .parse(content, None)
            .ok_or_else(|| SemanticError::ParseError {
                path: path.to_path_buf(),
                detail: "tree-sitter returned no tree".to_string(),
            })?;

        Ok(extractor.extract_symbols(&tree, content, path))
    }

    /// Check if the given file path has a supported language.
    #[must_use]
    pub fn supports_language(&self, path: &Path) -> bool {
        self.resolve_index(path).is_some()
    }

    /// Check if `content` has syntax errors when parsed with the grammar for
    /// `path`. Returns `true` if the parse tree contains `ERROR` or `MISSING`
    /// nodes, indicating that the content is not syntactically valid.
    ///
    /// Returns `false` for unsupported languages (no grammar to check against).
    #[must_use]
    pub fn has_syntax_errors(&self, path: &Path, content: &[u8]) -> bool {
        let idx = match self.resolve_index(path) {
            Some(i) => i,
            None => return false,
        };

        let language = self.extractors[idx].language();
        let mut ts_parser = tree_sitter::Parser::new();
        if ts_parser.set_language(&language).is_err() {
            return false;
        }

        match ts_parser.parse(content, None) {
            Some(tree) => tree_has_errors(&tree),
            None => true, // parse failure counts as an error
        }
    }
}

/// Recursively check if a tree-sitter tree contains ERROR or MISSING nodes.
fn tree_has_errors(tree: &tree_sitter::Tree) -> bool {
    let root = tree.root_node();
    // Fast path: tree-sitter sets has_error on ancestor nodes.
    if !root.has_error() {
        return false;
    }
    node_has_error(&root)
}

/// Walk the tree looking for ERROR or MISSING nodes.
fn node_has_error(node: &tree_sitter::Node) -> bool {
    if node.is_error() || node.is_missing() {
        return true;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.has_error() && node_has_error(&child) {
            return true;
        }
    }
    false
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
