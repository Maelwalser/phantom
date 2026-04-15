//! Error types for the `phantom-events` crate.

use phantom_core::error::CoreError;

/// Errors originating from event store operations.
#[derive(Debug, thiserror::Error)]
pub enum EventStoreError {
    /// SQLite operation failed (via sqlx).
    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// A stored timestamp could not be parsed as RFC 3339.
    #[error("invalid timestamp '{0}': {1}")]
    InvalidTimestamp(String, String),

    /// The database schema version is incompatible with this binary.
    #[error("schema version mismatch: expected {expected}, found {found}")]
    SchemaMismatch {
        /// The version this binary supports.
        expected: u32,
        /// The version found in the database.
        found: u32,
    },

    /// A projection snapshot could not be deserialized or serialized.
    #[error("snapshot corrupted: {0}")]
    SnapshotCorrupted(String),

    /// An error from phantom-core.
    #[error("core error: {0}")]
    Core(#[from] CoreError),
}

impl From<EventStoreError> for CoreError {
    fn from(err: EventStoreError) -> Self {
        match &err {
            EventStoreError::Serialization(_) | EventStoreError::InvalidTimestamp(..) => {
                CoreError::Serialization(err.to_string())
            }
            EventStoreError::SchemaMismatch { .. } => CoreError::Storage(err.to_string()),
            _ => CoreError::Storage(err.to_string()),
        }
    }
}
