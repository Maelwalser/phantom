//! Error types for git operations.

/// Errors produced by git operations.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// A `git2` operation failed.
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    /// A requested resource (commit, file, path) was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A materialization or tree-building operation failed.
    #[error("materialization failed: {0}")]
    MaterializationFailed(String),

    /// A standard I/O error occurred.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
