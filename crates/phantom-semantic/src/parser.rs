//! Tree-sitter parsing and language-aware symbol extraction.
//!
//! The [`Parser`] holds registered language extractors and delegates symbol
//! extraction to the appropriate one based on file extension.

use std::collections::HashMap;
use std::path::Path;

use phantom_core::SymbolEntry;

use crate::error::SemanticError;
use crate::languages::LanguageExtractor;
use crate::languages::go::GoExtractor;
use crate::languages::python::PythonExtractor;
use crate::languages::rust::RustExtractor;
use crate::languages::typescript::TypeScriptExtractor;

/// Multi-language parser that routes files to the right tree-sitter grammar.
pub struct Parser {
    /// Maps file extension to extractor index.
    ext_to_index: HashMap<String, usize>,
    /// Registered extractors.
    extractors: Vec<Box<dyn LanguageExtractor>>,
}

impl Parser {
    /// Create a new parser with all built-in language extractors registered.
    #[must_use]
    pub fn new() -> Self {
        let mut parser = Self {
            ext_to_index: HashMap::new(),
            extractors: Vec::new(),
        };
        parser.register(Box::new(RustExtractor));
        parser.register(Box::new(TypeScriptExtractor::new()));
        parser.register(Box::new(TypeScriptExtractor::tsx()));
        parser.register(Box::new(PythonExtractor));
        parser.register(Box::new(GoExtractor));
        parser
    }

    /// Register a language extractor, mapping each of its extensions.
    fn register(&mut self, extractor: Box<dyn LanguageExtractor>) {
        let idx = self.extractors.len();
        for ext in extractor.extensions() {
            self.ext_to_index.insert(ext.to_string(), idx);
        }
        self.extractors.push(extractor);
    }

    /// Parse a file and extract its symbols.
    pub fn parse_file(
        &self,
        path: &Path,
        content: &[u8],
    ) -> Result<Vec<SymbolEntry>, SemanticError> {
        let ext = path.extension().and_then(|e| e.to_str()).ok_or_else(|| {
            SemanticError::UnsupportedLanguage {
                path: path.to_path_buf(),
            }
        })?;

        let idx = self
            .ext_to_index
            .get(ext)
            .ok_or_else(|| SemanticError::UnsupportedLanguage {
                path: path.to_path_buf(),
            })?;

        let extractor = &self.extractors[*idx];
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
        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| self.ext_to_index.contains_key(ext))
            .unwrap_or(false)
    }

    /// Check if `content` has syntax errors when parsed with the grammar for
    /// `path`. Returns `true` if the parse tree contains `ERROR` or `MISSING`
    /// nodes, indicating that the content is not syntactically valid.
    ///
    /// Returns `false` for unsupported languages (no grammar to check against).
    #[must_use]
    pub fn has_syntax_errors(&self, path: &Path, content: &[u8]) -> bool {
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => return false,
        };
        let idx = match self.ext_to_index.get(ext) {
            Some(i) => *i,
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
mod tests {
    use super::*;
    use phantom_core::symbol::SymbolKind;

    #[test]
    fn parses_rust_file() {
        let parser = Parser::new();
        let src = b"fn hello() {}";
        let symbols = parser.parse_file(Path::new("test.rs"), src).unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn parses_typescript_file() {
        let parser = Parser::new();
        let src = b"function greet(): void {}";
        let symbols = parser.parse_file(Path::new("test.ts"), src).unwrap();
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function));
    }

    #[test]
    fn parses_python_file() {
        let parser = Parser::new();
        let src = b"def hello():\n    pass";
        let symbols = parser.parse_file(Path::new("test.py"), src).unwrap();
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Function));
    }

    #[test]
    fn unsupported_extension_errors() {
        let parser = Parser::new();
        let result = parser.parse_file(Path::new("test.txt"), b"hello");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            SemanticError::UnsupportedLanguage { .. }
        ));
    }

    #[test]
    fn supports_language_checks() {
        let parser = Parser::new();
        assert!(parser.supports_language(Path::new("foo.rs")));
        assert!(parser.supports_language(Path::new("bar.ts")));
        assert!(parser.supports_language(Path::new("baz.py")));
        assert!(parser.supports_language(Path::new("qux.go")));
        assert!(parser.supports_language(Path::new("comp.tsx")));
        assert!(!parser.supports_language(Path::new("readme.md")));
        assert!(!parser.supports_language(Path::new("noext")));
    }
}
