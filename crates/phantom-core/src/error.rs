//! Error types for `phantom-core`.
//!
//! Each crate in the Phantom workspace defines its own error enum.
//! [`CoreError`] covers failures that can originate from core type
//! operations shared across all crates.

use crate::changeset::ChangesetStatus;
use crate::id::{AgentId, ChangesetId};

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
}
