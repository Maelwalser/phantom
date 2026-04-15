//! FUSE filesystem adapter for the copy-on-write overlay.
//!
//! `PhantomFs` wraps an `OverlayLayer` and exposes it as a FUSE
//! filesystem via the `fuser` crate. This module is only compiled on Linux.

#[cfg(target_os = "linux")]
mod inner {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::fs::OpenOptions;
    use std::os::unix::fs::{FileExt, PermissionsExt};
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use fuser::{
        BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation,
        INodeNo, LockOwner, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData,
        ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, WriteFlags,
    };
    use phantom_core::AgentId;
    use tracing::{debug, warn};

    use crate::inode_table::InodeTable;
    use crate::layer::OverlayLayer;

    /// An open file descriptor tracked by the FUSE filesystem.
    ///
    /// Created on `open()` or `create()`, used for `pread`/`pwrite` during
    /// `read()`/`write()`, and dropped on `release()`.
    struct OpenFile {
        file: std::fs::File,
        writable: bool,
    }

    /// TTL for attribute and entry caching (1 second).
    const TTL: Duration = Duration::from_secs(1);

    /// Snapshotted directory listing for a single `opendir` handle.
    ///
    /// Entries are captured at `opendir` time and each assigned a sequential
    /// 1-based offset cookie.  This eliminates hash-collision bugs that occur
    /// when using filename hashes as resumption cookies.
    struct DirSnapshot {
        /// `(inode, file_type, name)` — order is fixed at snapshot time.
        entries: Vec<(u64, FileType, String)>,
    }

    /// FUSE filesystem backed by an [`OverlayLayer`].
    pub struct PhantomFs {
        layer: RwLock<OverlayLayer>,
        agent_id: AgentId,
        inodes: InodeTable,
        /// Counter for allocating unique file handles (shared for files and dirs).
        next_fh: AtomicU64,
        /// Open file descriptor table. Keyed by the file handle returned to
        /// the kernel via `open()` / `create()`.
        open_files: RwLock<HashMap<u64, OpenFile>>,
        /// Open directory handles. Keyed by the file handle returned via
        /// `opendir()`.  Each entry holds a snapshotted listing so that
        /// paginated `readdir` calls use collision-free sequential offsets.
        open_dirs: RwLock<HashMap<u64, DirSnapshot>>,
    }

    impl PhantomFs {
        /// Create a new FUSE filesystem for the given agent.
        pub fn new(layer: OverlayLayer, agent_id: AgentId) -> Self {
            Self {
                layer: RwLock::new(layer),
                agent_id,
                inodes: InodeTable::new(),
                next_fh: AtomicU64::new(1),
                open_files: RwLock::new(HashMap::new()),
                open_dirs: RwLock::new(HashMap::new()),
            }
        }

        /// Return the agent ID this filesystem belongs to.
        #[must_use]
        pub fn agent_id(&self) -> &AgentId {
            &self.agent_id
        }

        /// Purge stale unlinked inodes whose `forget` was never dispatched.
        ///
        /// Returns the number of inodes purged. Call this during overlay
        /// teardown to prevent memory leaks from unclean unmounts or agent
        /// crashes.
        pub fn purge_stale_inodes(&self) -> usize {
            let purged = self.inodes.purge_unlinked();
            if purged > 0 {
                debug!(purged, agent = %self.agent_id, "purged stale unlinked inodes");
            }
            purged
        }
    }

    /// Convert [`std::fs::Metadata`] to [`fuser::FileAttr`].
    fn metadata_to_attr(ino: u64, meta: &std::fs::Metadata) -> FileAttr {
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

        FileAttr {
            ino: INodeNo(ino),
            size: meta.len(),
            blocks: meta.len().div_ceil(512),
            atime,
            mtime,
            ctime,
            crtime: UNIX_EPOCH,
            kind,
            perm: (meta.permissions().mode() as u16) & 0o7777,
            nlink: if meta.is_dir() { 2 } else { 1 },
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// Build a default directory attr for the root or when metadata is unavailable.
    fn default_dir_attr(ino: u64) -> FileAttr {
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
            perm: 0o755,
            nlink: 2,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    impl Filesystem for PhantomFs {
        fn forget(&self, _req: &Request, ino: INodeNo, nlookup: u64) {
            debug!(ino = ino.0, nlookup, "forget");
            self.inodes.forget(ino.0, nlookup);
        }

        fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
            let Some(parent_path) = self.inodes.get_path(parent.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let child_path = parent_path.join(name);
            let layer = self.layer.read().unwrap();

            match layer.getattr(&child_path) {
                Ok(meta) => {
                    let ino = self.inodes.get_or_create_inode(&child_path);
                    let attr = metadata_to_attr(ino, &meta);
                    reply.entry(&TTL, &attr, Generation(0));
                }
                Err(_) => {
                    reply.error(Errno::ENOENT);
                }
            }
        }

        fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
            let Some(path) = self.inodes.get_path(ino.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            // Root directory special case.
            if ino.0 == 1 {
                let layer = self.layer.read().unwrap();
                match layer.getattr(&path) {
                    Ok(meta) => reply.attr(&TTL, &metadata_to_attr(ino.0, &meta)),
                    Err(_) => reply.attr(&TTL, &default_dir_attr(ino.0)),
                }
                return;
            }

            let layer = self.layer.read().unwrap();
            match layer.getattr(&path) {
                Ok(meta) => {
                    let mut attr = metadata_to_attr(ino.0, &meta);
                    if self.inodes.is_unlinked(ino.0) {
                        attr.nlink = 0;
                    }
                    reply.attr(&TTL, &attr);
                }
                Err(_) => reply.error(Errno::ENOENT),
            }
        }

        fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
            let Some(path) = self.inodes.get_path(ino.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            // Snapshot the directory listing at open time so paginated readdir
            // calls use collision-free sequential offsets.
            let entries = {
                let layer = self.layer.read().unwrap();
                match layer.read_dir(&path) {
                    Ok(e) => e,
                    Err(_) => {
                        reply.error(Errno::ENOENT);
                        return;
                    }
                }
            };

            let parent_ino = if ino.0 == 1 {
                1
            } else {
                path.parent()
                    .map(|p| self.inodes.get_or_create_inode(&p.to_path_buf()))
                    .unwrap_or(1)
            };

            let mut all_entries: Vec<(u64, FileType, String)> = vec![
                (ino.0, FileType::Directory, ".".to_string()),
                (parent_ino, FileType::Directory, "..".to_string()),
            ];

            for entry in &entries {
                let child_path = path.join(&entry.name);
                let child_ino = self.inodes.get_or_create_inode(&child_path);
                let ft = match entry.file_type {
                    crate::types::FileType::File => FileType::RegularFile,
                    crate::types::FileType::Directory => FileType::Directory,
                    crate::types::FileType::Symlink => FileType::Symlink,
                };
                all_entries.push((child_ino, ft, entry.name.to_string_lossy().into_owned()));
            }

            // Sort by name for deterministic order.
            all_entries.sort_by(|a, b| a.2.cmp(&b.2));

            let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
            self.open_dirs.write().unwrap().insert(
                fh,
                DirSnapshot {
                    entries: all_entries,
                },
            );
            reply.opened(FileHandle(fh), FopenFlags::empty());
        }

        fn readdir(
            &self,
            _req: &Request,
            _ino: INodeNo,
            fh: FileHandle,
            offset: u64,
            mut reply: ReplyDirectory,
        ) {
            let dirs = self.open_dirs.read().unwrap();
            let Some(snapshot) = dirs.get(&fh.0) else {
                reply.error(Errno::EBADF);
                return;
            };

            // offset is the sequential 1-based cookie of the last entry
            // returned.  Entries are numbered 1..=len, so offset==0 means
            // "start from the beginning" and offset==N means "resume after
            // the Nth entry".
            let start = offset as usize;
            for (idx, (child_ino, ft, name)) in snapshot.entries.iter().enumerate().skip(start) {
                // Cookie for this entry: 1-based index.
                let cookie = (idx as u64) + 1;
                if reply.add(INodeNo(*child_ino), cookie, *ft, name) {
                    break;
                }
            }
            reply.ok();
        }

        fn releasedir(
            &self,
            _req: &Request,
            _ino: INodeNo,
            fh: FileHandle,
            _flags: OpenFlags,
            reply: ReplyEmpty,
        ) {
            self.open_dirs.write().unwrap().remove(&fh.0);
            reply.ok();
        }

        fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
            let Some(path) = self.inodes.get_path(ino.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let raw = flags.0;
            let writable = (raw & libc::O_WRONLY != 0) || (raw & libc::O_RDWR != 0);
            let truncate = raw & libc::O_TRUNC != 0;

            let real_path = if writable {
                let mut layer = self.layer.write().unwrap();
                match layer.ensure_upper_copy(&path) {
                    Ok(p) => p,
                    Err(_) => {
                        reply.error(Errno::ENOENT);
                        return;
                    }
                }
            } else {
                let layer = self.layer.read().unwrap();
                match layer.resolve_path(&path) {
                    Ok(p) => p,
                    Err(_) => {
                        reply.error(Errno::ENOENT);
                        return;
                    }
                }
            };

            let file = if writable {
                OpenOptions::new().read(true).write(true).open(&real_path)
            } else {
                OpenOptions::new().read(true).open(&real_path)
            };

            let file = match file {
                Ok(f) => f,
                Err(e) => {
                    warn!(error = %e, "open: failed to open backing file");
                    reply.error(Errno::EIO);
                    return;
                }
            };

            if truncate && let Err(e) = file.set_len(0) {
                warn!(error = %e, "open: truncate failed");
                reply.error(Errno::EIO);
                return;
            }

            let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
            self.open_files
                .write()
                .unwrap()
                .insert(fh, OpenFile { file, writable });
            reply.opened(FileHandle(fh), FopenFlags::empty());
        }

        fn read(
            &self,
            _req: &Request,
            _ino: INodeNo,
            fh: FileHandle,
            offset: u64,
            size: u32,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: ReplyData,
        ) {
            let open_files = self.open_files.read().unwrap();
            let Some(open_file) = open_files.get(&fh.0) else {
                reply.error(Errno::EBADF);
                return;
            };

            let mut buf = vec![0u8; size as usize];
            match open_file.file.read_at(&mut buf, offset) {
                Ok(0) => reply.data(&[]),
                Ok(n) => reply.data(&buf[..n]),
                Err(e) => {
                    warn!(error = %e, "read: pread failed");
                    reply.error(Errno::EIO);
                }
            }
        }

        fn write(
            &self,
            _req: &Request,
            _ino: INodeNo,
            fh: FileHandle,
            offset: u64,
            data: &[u8],
            _write_flags: WriteFlags,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: ReplyWrite,
        ) {
            let open_files = self.open_files.read().unwrap();
            let Some(open_file) = open_files.get(&fh.0) else {
                reply.error(Errno::EBADF);
                return;
            };

            if !open_file.writable {
                reply.error(Errno::EBADF);
                return;
            }

            // Extend file if writing past current end (creates a sparse hole).
            let write_end = offset + data.len() as u64;
            if let Ok(meta) = open_file.file.metadata()
                && write_end > meta.len()
                && let Err(e) = open_file.file.set_len(write_end)
            {
                warn!(error = %e, "write: set_len failed");
                reply.error(Errno::EIO);
                return;
            }

            match open_file.file.write_at(data, offset) {
                Ok(n) => reply.written(n as u32),
                Err(e) => {
                    warn!(error = %e, "write: pwrite failed");
                    reply.error(Errno::EIO);
                }
            }
        }

        fn create(
            &self,
            _req: &Request,
            parent: INodeNo,
            name: &OsStr,
            _mode: u32,
            _umask: u32,
            _flags: i32,
            reply: ReplyCreate,
        ) {
            let Some(parent_path) = self.inodes.get_path(parent.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let child_path = parent_path.join(name);
            let mut layer = self.layer.write().unwrap();

            match layer.write_file(&child_path, &[]) {
                Ok(()) => {
                    layer.remove_whiteout(&child_path);

                    // Open a real file handle so subsequent writes use pwrite.
                    let real_path = match layer.resolve_path(&child_path) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(error = %e, "create: resolve after write failed");
                            reply.error(Errno::EIO);
                            return;
                        }
                    };
                    let ino = self.inodes.get_or_create_inode(&child_path);
                    drop(layer);

                    let file = match OpenOptions::new().read(true).write(true).open(&real_path) {
                        Ok(f) => f,
                        Err(e) => {
                            warn!(error = %e, "create: open backing file failed");
                            reply.error(Errno::EIO);
                            return;
                        }
                    };

                    let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
                    self.open_files.write().unwrap().insert(
                        fh,
                        OpenFile {
                            file,
                            writable: true,
                        },
                    );
                    let now = SystemTime::now();
                    let attr = FileAttr {
                        ino: INodeNo(ino),
                        size: 0,
                        blocks: 0,
                        atime: now,
                        mtime: now,
                        ctime: now,
                        crtime: UNIX_EPOCH,
                        kind: FileType::RegularFile,
                        perm: 0o644,
                        nlink: 1,
                        uid: unsafe { libc::getuid() },
                        gid: unsafe { libc::getgid() },
                        rdev: 0,
                        blksize: 4096,
                        flags: 0,
                    };
                    reply.created(
                        &TTL,
                        &attr,
                        Generation(0),
                        FileHandle(fh),
                        FopenFlags::empty(),
                    );
                }
                Err(e) => {
                    warn!(error = %e, "create failed");
                    reply.error(Errno::EIO);
                }
            }
        }

        fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
            let Some(parent_path) = self.inodes.get_path(parent.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let child_path = parent_path.join(name);
            let mut layer = self.layer.write().unwrap();

            match layer.delete_file(&child_path) {
                Ok(()) => {
                    self.inodes.unlink(&child_path);
                    drop(layer);
                    reply.ok();
                }
                Err(e) => {
                    warn!(error = %e, "unlink failed");
                    reply.error(Errno::EIO);
                }
            }
        }

        fn setattr(
            &self,
            _req: &Request,
            ino: INodeNo,
            mode: Option<u32>,
            _uid: Option<u32>,
            _gid: Option<u32>,
            size: Option<u64>,
            _atime: Option<fuser::TimeOrNow>,
            _mtime: Option<fuser::TimeOrNow>,
            _ctime: Option<SystemTime>,
            _fh: Option<FileHandle>,
            _crtime: Option<SystemTime>,
            _chgtime: Option<SystemTime>,
            _bkuptime: Option<SystemTime>,
            _flags: Option<BsdFileFlags>,
            reply: ReplyAttr,
        ) {
            let Some(path) = self.inodes.get_path(ino.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let needs_write = mode.is_some() || size.is_some();

            if needs_write {
                // Single write lock for all mutations + final getattr.
                let mut layer = self.layer.write().unwrap();

                if let Some(new_mode) = mode
                    && let Err(e) = layer.set_permissions(&path, new_mode)
                {
                    warn!(error = %e, "setattr chmod failed");
                    reply.error(Errno::EIO);
                    return;
                }

                if let Some(new_size) = size
                    && let Err(e) = layer.truncate_file(&path, new_size)
                {
                    warn!(error = %e, "setattr truncate failed");
                    reply.error(Errno::EIO);
                    return;
                }

                match layer.getattr(&path) {
                    Ok(meta) => reply.attr(&TTL, &metadata_to_attr(ino.0, &meta)),
                    Err(_) => reply.error(Errno::ENOENT),
                }
            } else {
                // Read-only: just fetch attributes.
                let layer = self.layer.read().unwrap();
                match layer.getattr(&path) {
                    Ok(meta) => reply.attr(&TTL, &metadata_to_attr(ino.0, &meta)),
                    Err(_) => reply.error(Errno::ENOENT),
                }
            }
        }

        fn mkdir(
            &self,
            _req: &Request,
            parent: INodeNo,
            name: &OsStr,
            _mode: u32,
            _umask: u32,
            reply: ReplyEntry,
        ) {
            let Some(parent_path) = self.inodes.get_path(parent.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let child_path = parent_path.join(name);
            let mut layer = self.layer.write().unwrap();

            // Passthrough paths create directories directly in the lower layer.
            let target_path = if layer.is_passthrough(&child_path) {
                layer.lower_dir().join(&child_path)
            } else {
                layer.upper_dir().join(&child_path)
            };

            match std::fs::create_dir_all(&target_path) {
                Ok(()) => {
                    if !layer.is_passthrough(&child_path) {
                        // Clear whiteout if this dir was previously deleted.
                        layer.remove_whiteout(&child_path);
                    }
                    let ino = self.inodes.get_or_create_inode(&child_path);
                    reply.entry(&TTL, &default_dir_attr(ino), Generation(0));
                    debug!(path = %child_path.display(), "mkdir");
                }
                Err(e) => {
                    warn!(error = %e, "mkdir failed");
                    reply.error(Errno::EIO);
                }
            }
        }

        fn rename(
            &self,
            _req: &Request,
            parent: INodeNo,
            name: &OsStr,
            newparent: INodeNo,
            newname: &OsStr,
            flags: RenameFlags,
            reply: ReplyEmpty,
        ) {
            // Atomic exchange is not supported.
            if flags.contains(RenameFlags::RENAME_EXCHANGE) {
                reply.error(Errno::ENOSYS);
                return;
            }

            let Some(old_parent_path) = self.inodes.get_path(parent.0) else {
                reply.error(Errno::ENOENT);
                return;
            };
            let Some(new_parent_path) = self.inodes.get_path(newparent.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let old_path = old_parent_path.join(name);
            let new_path = new_parent_path.join(newname);

            let mut layer = self.layer.write().unwrap();

            // RENAME_NOREPLACE: fail if destination already exists.
            if flags.contains(RenameFlags::RENAME_NOREPLACE) && layer.exists(&new_path) {
                reply.error(Errno::EEXIST);
                return;
            }

            match layer.rename_file(&old_path, &new_path) {
                Ok(()) => {
                    self.inodes.rename(&old_path, &new_path);
                    drop(layer);
                    reply.ok();
                }
                Err(crate::error::OverlayError::PathNotFound(_)) => {
                    reply.error(Errno::ENOENT);
                }
                Err(crate::error::OverlayError::Io(ref e))
                    if e.raw_os_error() == Some(libc::ENOTEMPTY) =>
                {
                    reply.error(Errno::ENOTEMPTY);
                }
                Err(e) => {
                    warn!(error = %e, "rename failed");
                    reply.error(Errno::EIO);
                }
            }
        }

        fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
            let Some(parent_path) = self.inodes.get_path(parent.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let child_path = parent_path.join(name);
            let mut layer = self.layer.write().unwrap();

            // Passthrough directories (e.g. .git) must not be removed via the overlay.
            if layer.is_passthrough(&child_path) {
                reply.error(Errno::EPERM);
                return;
            }

            // Check the merged view — the directory must exist in the overlay.
            if !layer.exists(&child_path) {
                reply.error(Errno::ENOENT);
                return;
            }

            // POSIX: rmdir must fail with ENOTEMPTY if the directory is non-empty.
            // Evaluate the merged view (upper + lower minus whiteouts).
            match layer.read_dir(&child_path) {
                Ok(entries) if !entries.is_empty() => {
                    reply.error(Errno::ENOTEMPTY);
                    return;
                }
                Err(_) => {
                    reply.error(Errno::EIO);
                    return;
                }
                Ok(_) => {} // empty — proceed
            }

            // Remove the upper-layer copy if it exists.
            let upper_path = layer.upper_dir().join(&child_path);
            if upper_path.is_dir()
                && let Err(e) = std::fs::remove_dir(&upper_path)
            {
                warn!(error = %e, "rmdir: failed to remove upper directory");
                reply.error(Errno::EIO);
                return;
            }

            // Write whiteout to hide any lower-layer copy.
            let _ = layer.delete_file(&child_path);
            self.inodes.unlink(&child_path);
            drop(layer);
            reply.ok();
        }

        fn release(
            &self,
            _req: &Request,
            _ino: INodeNo,
            fh: FileHandle,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            _flush: bool,
            reply: ReplyEmpty,
        ) {
            self.open_files.write().unwrap().remove(&fh.0);
            reply.ok();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Helper: simulate paginated readdir over a snapshot, collecting all
        /// returned entries.  Mimics the kernel calling readdir repeatedly with
        /// the last cookie returned.
        fn collect_readdir(snapshot: &DirSnapshot, page_size: usize) -> Vec<String> {
            let mut result = Vec::new();
            let mut offset: u64 = 0;
            loop {
                let start = offset as usize;
                let mut added = 0;
                for (idx, (_ino, _ft, name)) in snapshot.entries.iter().enumerate().skip(start) {
                    let cookie = (idx as u64) + 1;
                    result.push(name.clone());
                    offset = cookie;
                    added += 1;
                    if added >= page_size {
                        break;
                    }
                }
                if added == 0 {
                    break;
                }
            }
            result
        }

        #[test]
        fn readdir_sequential_returns_all_entries() {
            let snapshot = DirSnapshot {
                entries: vec![
                    (1, FileType::Directory, ".".into()),
                    (1, FileType::Directory, "..".into()),
                    (2, FileType::RegularFile, "a.txt".into()),
                    (3, FileType::RegularFile, "b.txt".into()),
                    (4, FileType::RegularFile, "c.txt".into()),
                ],
            };

            // Page size of 2 forces multiple readdir rounds.
            let names = collect_readdir(&snapshot, 2);
            assert_eq!(names, vec![".", "..", "a.txt", "b.txt", "c.txt"]);
        }

        #[test]
        fn readdir_single_page_returns_all() {
            let snapshot = DirSnapshot {
                entries: vec![
                    (1, FileType::Directory, ".".into()),
                    (1, FileType::Directory, "..".into()),
                    (2, FileType::RegularFile, "only.txt".into()),
                ],
            };

            let names = collect_readdir(&snapshot, 100);
            assert_eq!(names, vec![".", "..", "only.txt"]);
        }

        #[test]
        fn readdir_empty_directory() {
            let snapshot = DirSnapshot {
                entries: vec![
                    (1, FileType::Directory, ".".into()),
                    (1, FileType::Directory, "..".into()),
                ],
            };

            let names = collect_readdir(&snapshot, 1);
            assert_eq!(names, vec![".", ".."]);
        }

        #[test]
        fn readdir_page_size_one_returns_all() {
            let snapshot = DirSnapshot {
                entries: vec![
                    (1, FileType::Directory, ".".into()),
                    (1, FileType::Directory, "..".into()),
                    (10, FileType::RegularFile, "x".into()),
                    (11, FileType::RegularFile, "y".into()),
                    (12, FileType::RegularFile, "z".into()),
                ],
            };

            // Page size 1 = worst-case pagination.
            let names = collect_readdir(&snapshot, 1);
            assert_eq!(names, vec![".", "..", "x", "y", "z"]);
        }

        #[test]
        fn readdir_no_duplicate_entries() {
            let entries: Vec<(u64, FileType, String)> = (0..50)
                .map(|i| (i + 2, FileType::RegularFile, format!("file_{i:04}.txt")))
                .collect();
            let mut all: Vec<(u64, FileType, String)> = vec![
                (1, FileType::Directory, ".".into()),
                (1, FileType::Directory, "..".into()),
            ];
            all.extend(entries);
            let snapshot = DirSnapshot { entries: all };

            let names = collect_readdir(&snapshot, 7);
            // Verify no duplicates and correct count.
            assert_eq!(names.len(), 52);
            let unique: std::collections::HashSet<&String> = names.iter().collect();
            assert_eq!(unique.len(), 52, "readdir produced duplicate entries");
        }
    }
}

#[cfg(target_os = "linux")]
pub use inner::PhantomFs;
