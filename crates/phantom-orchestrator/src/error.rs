//! Error types for the orchestrator crate.

/// Errors produced by orchestrator operations (git, materialization, scheduling).
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    /// A git operation failed.
    #[error(transparent)]
    Git(#[from] phantom_git::error::GitError),

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

    /// Materialization failed AND the recovery/rollback also failed, leaving
    /// the trunk working tree in an indeterminate state.
    #[error(
        "materialization failed: {cause}; RECOVERY ALSO FAILED — trunk may be corrupt: {recovery_errors}"
    )]
    MaterializationRecoveryFailed {
        cause: String,
        recovery_errors: String,
    },

    /// A requested resource (commit, file, changeset) was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A live rebase operation failed.
    #[error("live rebase error: {0}")]
    LiveRebase(String),

    /// Post-materialization integrity check failed.
    ///
    /// Emitted when, after a Phantom operation, the trunk git repository can
    /// no longer be opened (e.g. `.git/HEAD` or `.git/config` went missing).
    /// Phantom stops immediately rather than continue operating on a
    /// corrupted repository.
    #[error("repository integrity violated after Phantom operation: {0}")]
    IntegrityViolation(String),
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

impl From<git2::Error> for OrchestratorError {
    fn from(e: git2::Error) -> Self {
        OrchestratorError::Git(phantom_git::error::GitError::Git(e))
    }
}
