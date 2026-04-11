//! Error types for `phantom-semantic`.

use std::path::PathBuf;

/// Errors originating from semantic analysis operations.
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    /// tree-sitter failed to parse a file.
    #[error("parse error in {path}: {detail}")]
    ParseError {
        /// The file that could not be parsed.
        path: PathBuf,
        /// Human-readable description of the failure.
        detail: String,
    },

    /// The file's language is not supported by any registered extractor.
    #[error("unsupported language for file: {path}")]
    UnsupportedLanguage {
        /// The file whose extension was not recognized.
        path: PathBuf,
    },

    /// An I/O error occurred while reading files for indexing.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// An error during the merge process.
    #[error("merge error: {0}")]
    MergeError(String),
}
