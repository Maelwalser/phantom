//! Filesystem identity and permission configuration.

/// Configuration for the projected filesystem identity and permissions.
///
/// Controls the UID/GID and permission modes reported by the FUSE
/// filesystem. When `MountOption::DefaultPermissions` is set, the kernel
/// enforces access checks based on these values — projecting a restricted
/// UID/GID creates a real privilege boundary between the agent process and
/// the overlay.
pub struct FsConfig {
    /// Owner UID projected for all files and directories.
    pub uid: u32,
    /// Owner GID projected for all files and directories.
    pub gid: u32,
    /// Permission mode for regular files (default: `0o644`).
    pub file_mode: u16,
    /// Permission mode for directories (default: `0o755`).
    pub dir_mode: u16,
}

impl Default for FsConfig {
    /// Returns a config that inherits the current process's UID/GID.
    ///
    /// This preserves backward-compatible behavior but provides no
    /// privilege separation. For production deployments, configure
    /// explicit UID/GID values for sandboxed agent identities.
    fn default() -> Self {
        // SAFETY: getuid/getgid are always safe — they read process
        // credentials with no side effects.
        Self {
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            file_mode: 0o644,
            dir_mode: 0o755,
        }
    }
}
