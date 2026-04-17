//! Convert [`std::fs::Metadata`] into [`fuser::FileAttr`] for FUSE replies.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{FileAttr, FileType, INodeNo};

use super::config::FsConfig;

/// TTL for attribute and entry caching (1 second).
pub(super) const TTL: Duration = Duration::from_secs(1);

/// Convert [`std::fs::Metadata`] to [`fuser::FileAttr`].
pub(super) fn metadata_to_attr(ino: u64, meta: &std::fs::Metadata, config: &FsConfig) -> FileAttr {
    use std::os::unix::fs::PermissionsExt;

    let kind = if meta.is_dir() {
        FileType::Directory
    } else if meta.is_symlink() {
        FileType::Symlink
    } else {
        FileType::RegularFile
    };

    let atime = meta.accessed().unwrap_or(UNIX_EPOCH);
    let mtime = meta.modified().unwrap_or(UNIX_EPOCH);
    let ctime = mtime; // Unix ctime ≈ mtime for our purposes.

    // Use real on-disk permissions so chmod is visible to FUSE clients.
    let perm = (meta.permissions().mode() & 0o7777) as u16;

    FileAttr {
        ino: INodeNo(ino),
        size: meta.len(),
        blocks: meta.len().div_ceil(512),
        atime,
        mtime,
        ctime,
        crtime: UNIX_EPOCH,
        kind,
        perm,
        nlink: if meta.is_dir() { 2 } else { 1 },
        uid: config.uid,
        gid: config.gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}

/// Build a default directory attr for the root or when metadata is unavailable.
pub(super) fn default_dir_attr(ino: u64, config: &FsConfig) -> FileAttr {
    let now = SystemTime::now();
    FileAttr {
        ino: INodeNo(ino),
        size: 0,
        blocks: 0,
        atime: now,
        mtime: now,
        ctime: now,
        crtime: UNIX_EPOCH,
        kind: FileType::Directory,
        perm: config.dir_mode,
        nlink: 2,
        uid: config.uid,
        gid: config.gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}
