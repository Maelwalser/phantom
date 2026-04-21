//! Three-way semantic merge engine.
//!
//! Implements [`phantom_core::traits::SemanticAnalyzer`] using tree-sitter
//! parsing and Weave-style entity matching.

mod conflict;
mod reconstruct;
mod text;

use std::path::Path;

use phantom_core::changeset::SemanticOperation;
use phantom_core::conflict::{MergeReport, MergeResult, MergeStrategy};
use phantom_core::error::CoreError;
use phantom_core::symbol::{SymbolEntry, SymbolReference};

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

    fn extract_references(
        &self,
        path: &Path,
        content: &[u8],
        _symbols: &[SymbolEntry],
    ) -> Result<Vec<SymbolReference>, CoreError> {
        // The parser re-extracts symbols inside `parse_file_with_refs` to
        // attribute references to their enclosing symbols. Taking `_symbols`
        // as a parameter keeps the trait signature uniform and future-proofs
        // for analyzers that can skip the second extraction.
        if !self.parser.supports_language(path) {
            return Ok(Vec::new());
        }
        self.parser
            .parse_file_with_refs(path, content)
            .map(|(_, refs)| refs)
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
            .map_or(Path::new("unknown"), |e| e.file.as_path());
        diff::diff_symbols(base, current, file)
    }

    fn three_way_merge(
        &self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        path: &Path,
    ) -> Result<MergeReport, CoreError> {
        // Short-circuit paths — conceptually semantically accurate, but no
        // parse happened, so tag them as Trivial to distinguish from the
        // full semantic path.
        if ours == theirs {
            return Ok(MergeReport::trivial(MergeResult::Clean(ours.to_vec())));
        }
        if ours == base {
            return Ok(MergeReport::trivial(MergeResult::Clean(theirs.to_vec())));
        }
        if theirs == base {
            return Ok(MergeReport::trivial(MergeResult::Clean(ours.to_vec())));
        }

        // Try semantic merge if language is supported.
        if self.parser.supports_language(path) {
            return if let Ok((result, strategy)) =
                conflict::semantic_merge(&self.parser, base, ours, theirs, path)
            {
                Ok(MergeReport { result, strategy })
            } else {
                tracing::warn!(?path, "semantic merge failed, falling back to text merge");
                Ok(MergeReport::text_fallback(
                    text::text_merge(base, ours, theirs, path),
                    MergeStrategy::TextFallbackSemanticError,
                ))
            };
        }

        tracing::info!(
            ?path,
            "unsupported language — using line-based text merge (no syntax validation)"
        );
        Ok(MergeReport::text_fallback(
            text::text_merge(base, ours, theirs, path),
            MergeStrategy::TextFallbackUnsupported,
        ))
    }

    fn supports_language(&self, path: &Path) -> bool {
        self.parser.supports_language(path)
    }
}

#[cfg(test)]
mod tests;
