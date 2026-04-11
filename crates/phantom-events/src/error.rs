//! Error types for the `phantom-events` crate.

use phantom_core::error::CoreError;

/// Errors originating from event store operations.
#[derive(Debug, thiserror::Error)]
pub enum EventStoreError {
    /// SQLite operation failed.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// JSON serialization or deserialization failed.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// An error from phantom-core.
    #[error("core error: {0}")]
    Core(#[from] CoreError),
}

impl From<EventStoreError> for CoreError {
    fn from(err: EventStoreError) -> Self {
        match &err {
            EventStoreError::Serialization(_) => CoreError::Serialization(err.to_string()),
            _ => CoreError::Storage(err.to_string()),
        }
    }
}
