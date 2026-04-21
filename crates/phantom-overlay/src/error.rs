//! Error types for `phantom-overlay`.

use std::path::PathBuf;

use phantom_core::AgentId;

/// Errors originating from overlay filesystem operations.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    /// An I/O operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// A FUSE-specific error occurred.
    #[error("fuse error: {0}")]
    Fuse(String),

    /// The requested agent overlay was not found.
    #[error("overlay not found for agent: {0}")]
    NotFound(AgentId),

    /// An overlay already exists for this agent.
    #[error("overlay already exists for agent: {0}")]
    AlreadyExists(AgentId),

    /// The inode number does not map to any known path.
    #[error("inode not found: {0}")]
    InodeNotFound(u64),

    /// The path does not exist in either overlay layer.
    #[error("path not found: {}", _0.display())]
    PathNotFound(PathBuf),

    /// Refused to write to a reserved path (`.git/`, `.phantom/`, or
    /// `.whiteouts.json` at any depth).
    ///
    /// Writing these paths would corrupt the user's git repository or
    /// Phantom's own state, so the overlay returns an error to the caller
    /// (mapped to `EACCES`/`ENOENT` at the FUSE boundary).
    #[error("refusing to write to reserved path: {}", _0.display())]
    ReservedPath(PathBuf),

    /// JSON serialization/deserialization of whiteout data failed.
    #[error("serialization error: {0}")]
    Serialization(String),
}
