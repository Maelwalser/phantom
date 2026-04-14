//! Three-way semantic merge engine.
//!
//! Implements [`phantom_core::traits::SemanticAnalyzer`] using tree-sitter
//! parsing and Weave-style entity matching.

mod conflict;
mod reconstruct;
mod text;

use std::path::Path;

use phantom_core::changeset::SemanticOperation;
use phantom_core::error::CoreError;
use phantom_core::symbol::SymbolEntry;
use phantom_core::traits::MergeResult;

use crate::diff;
use crate::parser::Parser;

/// Semantic merge engine backed by tree-sitter.
pub struct SemanticMerger {
    parser: Parser,
}

impl SemanticMerger {
    /// Create a new merger with the default parser.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
        }
    }
}

impl Default for SemanticMerger {
    fn default() -> Self {
        Self::new()
    }
}

impl phantom_core::traits::SemanticAnalyzer for SemanticMerger {
    fn extract_symbols(&self, path: &Path, content: &[u8]) -> Result<Vec<SymbolEntry>, CoreError> {
        self.parser
            .parse_file(path, content)
            .map_err(|e| CoreError::Semantic(e.to_string()))
    }

    fn diff_symbols(
        &self,
        base: &[SymbolEntry],
        current: &[SymbolEntry],
    ) -> Vec<SemanticOperation> {
        let file = base
            .first()
            .or(current.first())
            .map(|e| e.file.as_path())
            .unwrap_or(Path::new("unknown"));
        diff::diff_symbols(base, current, file)
    }

    fn three_way_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeResult, CoreError> {
        // If content is identical, short-circuit
        if ours == theirs {
            return Ok(MergeResult::Clean(ours.to_vec()));
        }
        if ours == base {
            return Ok(MergeResult::Clean(theirs.to_vec()));
        }
        if theirs == base {
            return Ok(MergeResult::Clean(ours.to_vec()));
        }

        // Try semantic merge if language is supported
        if self.parser.supports_language(path) {
            match conflict::semantic_merge(&self.parser, base, ours, theirs, path) {
                Ok(result) => return Ok(result),
                Err(_) => {
                    // Fall through to text-based merge
                    tracing::warn!(?path, "semantic merge failed, falling back to text merge");
                }
            }
        } else {
            tracing::info!(
                ?path,
                "unsupported language — using line-based text merge (no syntax validation)"
            );
        }

        // Fallback: line-based three-way merge
        text::text_merge(base, ours, theirs, path)
    }

    fn supports_language(&self, path: &Path) -> bool {
        self.parser.supports_language(path)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
