//! In-memory symbol index mapping files to symbols.
//!
//! Implements [`phantom_core::traits::SymbolIndex`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use phantom_core::id::{GitOid, SymbolId};
use phantom_core::symbol::SymbolEntry;
use phantom_core::traits::SymbolIndex;

use crate::error::SemanticError;
use crate::parser::Parser;

/// In-memory symbol index backed by hash maps.
pub struct InMemorySymbolIndex {
    symbols: HashMap<SymbolId, SymbolEntry>,
    file_to_symbols: HashMap<PathBuf, Vec<SymbolId>>,
    indexed_at: GitOid,
}

impl InMemorySymbolIndex {
    /// Create an empty index at the given commit.
    #[must_use]
    pub fn new(commit: GitOid) -> Self {
        Self {
            symbols: HashMap::new(),
            file_to_symbols: HashMap::new(),
            indexed_at: commit,
        }
    }

    /// Build an index by walking a directory and parsing all supported files.
    pub fn build_from_directory(
        root: &Path,
        parser: &Parser,
        commit: GitOid,
    ) -> Result<Self, SemanticError> {
        let mut index = Self::new(commit);
        walk_dir(root, root, parser, &mut index)?;
        Ok(index)
    }

    /// The commit this index was built from.
    #[must_use]
    pub fn indexed_at(&self) -> GitOid {
        self.indexed_at
    }

    /// Number of symbols in the index.
    #[must_use]
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

impl SymbolIndex for InMemorySymbolIndex {
    fn lookup(&self, id: &SymbolId) -> Option<SymbolEntry> {
        self.symbols.get(id).cloned()
    }

    fn symbols_in_file(&self, path: &Path) -> Vec<SymbolEntry> {
        self.file_to_symbols
            .get(path)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.symbols.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn all_symbols(&self) -> Vec<SymbolEntry> {
        self.symbols.values().cloned().collect()
    }

    fn update_file(&mut self, path: &Path, symbols: Vec<SymbolEntry>) {
        // Remove old symbols for this file
        self.remove_file(path);

        // Insert new symbols
        let ids: Vec<SymbolId> = symbols.iter().map(|s| s.id.clone()).collect();
        for symbol in symbols {
            self.symbols.insert(symbol.id.clone(), symbol);
        }
        self.file_to_symbols.insert(path.to_path_buf(), ids);
    }

    fn remove_file(&mut self, path: &Path) {
        if let Some(ids) = self.file_to_symbols.remove(path) {
            for id in ids {
                self.symbols.remove(&id);
            }
        }
    }
}

/// Recursively walk a directory, parse supported files, and add symbols to the index.
fn walk_dir(
    dir: &Path,
    root: &Path,
    parser: &Parser,
    index: &mut InMemorySymbolIndex,
) -> Result<(), SemanticError> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories and common non-source dirs
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                walk_dir(&path, root, parser, index)?;
            }
        } else if parser.supports_language(&path) {
            let content = std::fs::read(&path)?;
            let relative = path.strip_prefix(root).unwrap_or(&path);
            match parser.parse_file(relative, &content) {
                Ok(symbols) => {
                    index.update_file(relative, symbols);
                }
                Err(SemanticError::ParseError { .. }) => {
                    // Log and skip files that fail to parse
                    tracing::warn!(?path, "skipping file that failed to parse");
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
