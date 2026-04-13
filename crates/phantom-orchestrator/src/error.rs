//! Error types for the orchestrator crate.

/// Errors produced by orchestrator operations (git, materialization, scheduling).
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    /// A `git2` operation failed.
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    /// The event store returned an error.
    #[error("event store error: {0}")]
    EventStore(String),

    /// A semantic analysis operation failed.
    #[error("semantic error: {0}")]
    Semantic(String),

    /// An overlay filesystem operation failed.
    #[error("overlay error: {0}")]
    Overlay(String),

    /// A standard I/O error occurred.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Materialization of a changeset to trunk failed.
    #[error("materialization failed: {0}")]
    MaterializationFailed(String),

    /// A requested resource (commit, file, changeset) was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A live rebase operation failed.
    #[error("live rebase error: {0}")]
    LiveRebase(String),
}

impl From<phantom_core::CoreError> for OrchestratorError {
    fn from(e: phantom_core::CoreError) -> Self {
        OrchestratorError::EventStore(e.to_string())
    }
}

impl From<phantom_semantic::SemanticError> for OrchestratorError {
    fn from(e: phantom_semantic::SemanticError) -> Self {
        OrchestratorError::Semantic(e.to_string())
    }
}
