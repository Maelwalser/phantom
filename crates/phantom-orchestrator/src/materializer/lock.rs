//! File-based exclusive lock that serializes concurrent materializations.
//!
//! `MaterializeLock::acquire` blocks until a unique lock on
//! `.phantom/submit.lock` is obtained. The lock is released via RAII when the
//! guard is dropped (flock is advisory and fd-based).

use std::path::Path;

use crate::error::OrchestratorError;

/// RAII guard for a file-based lock used to serialize materialization.
pub(super) struct MaterializeLock {
    _file: std::fs::File,
}

impl MaterializeLock {
    /// Acquire an exclusive lock on `.phantom/submit.lock`, blocking until
    /// any concurrent materialization finishes.
    #[allow(deprecated)] // nix 0.30 deprecates flock() in favor of Flock type
    pub(super) fn acquire(phantom_dir: &Path) -> Result<Self, OrchestratorError> {
        use std::os::unix::io::AsRawFd;
        let lock_path = phantom_dir.join("submit.lock");
        let file = std::fs::File::create(&lock_path)?;
        nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive).map_err(|e| {
            OrchestratorError::MaterializationFailed(format!("failed to acquire submit lock: {e}"))
        })?;
        Ok(Self { _file: file })
    }
}
