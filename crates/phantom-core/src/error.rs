//! Error types for `phantom-core`.
//!
//! Each crate in the Phantom workspace defines its own error enum.
//! [`CoreError`] covers failures that can originate from core type
//! operations shared across all crates.

use std::path::PathBuf;

use crate::changeset::ChangesetStatus;
use crate::id::{AgentId, ChangesetId};
use crate::reserved::ReservedPathKind;

/// Errors originating from core Phantom operations.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The requested changeset does not exist.
    #[error("changeset not found: {0}")]
    ChangesetNotFound(ChangesetId),

    /// The requested agent does not exist.
    #[error("agent not found: {0}")]
    AgentNotFound(AgentId),

    /// An illegal status transition was attempted.
    #[error("invalid status transition from {from:?} to {to:?}")]
    InvalidStatusTransition {
        /// Current status.
        from: ChangesetStatus,
        /// Attempted target status.
        to: ChangesetStatus,
    },

    /// Serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// Storage backend (event store, database) error.
    #[error("storage error: {0}")]
    Storage(String),

    /// Semantic analysis error.
    #[error("semantic error: {0}")]
    Semantic(String),

    /// A write targeted a path Phantom must never touch (`.git/`, `.phantom/`,
    /// or `.whiteouts.json`). Returned instead of corrupting the user's VCS.
    #[error("refusing to write to reserved path {path} ({kind:?})")]
    ReservedPath {
        /// The offending path.
        path: PathBuf,
        /// Which reserved-path rule matched.
        kind: ReservedPathKind,
    },

    /// Repository integrity check failed after a Phantom operation.
    ///
    /// Emitted when a post-write sanity check (e.g. `git2::Repository::open`
    /// on trunk) no longer succeeds. Phantom must stop immediately rather than
    /// continue operating on a corrupted repository.
    #[error("repository integrity violated: {0}")]
    IntegrityViolation(String),
}
