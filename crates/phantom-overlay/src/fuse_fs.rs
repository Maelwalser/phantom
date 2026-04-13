//! FUSE filesystem adapter for the copy-on-write overlay.
//!
//! `PhantomFs` wraps an `OverlayLayer` and exposes it as a FUSE
//! filesystem via the `fuser` crate. This module is only compiled on Linux.

#[cfg(target_os = "linux")]
mod inner {
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsStr;
    use std::fs::OpenOptions;
    use std::os::unix::fs::{FileExt, PermissionsExt};
    use std::path::PathBuf;
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

    /// All mutable inode state protected by a single `RwLock`.
    /// Read-only lookups (`get_path`, `is_unlinked`) take the read lock;
    /// mutations (`get_or_create_inode`, `unlink`, `rename`, `forget`)
    /// take the write lock.
    struct InodeTableInner {
        ino_to_path: HashMap<u64, PathBuf>,
        path_to_ino: HashMap<PathBuf, u64>,
        /// Kernel-side lookup reference count per inode.  Incremented on
        /// every `lookup`, `create`, `mkdir`, and `readdir` reply that
        /// hands an inode to the kernel; decremented by `forget`.
        lookup_count: HashMap<u64, u64>,
        /// Inodes that have been unlinked from the directory tree but still
        /// have a non-zero kernel lookup count.  The `ino_to_path` entry is
        /// kept alive so that open file descriptors can still resolve the
        /// inode.  `forget()` performs final cleanup when the count drops
        /// to zero.
        unlinked: HashSet<u64>,
    }

    /// Bidirectional map between inode numbers and filesystem paths.
    ///
    /// Tracks kernel lookup counts so that inodes can be evicted via the
    /// FUSE `forget` callback, preventing unbounded growth when large
    /// directory trees are traversed.
    ///
    /// All mutable state lives behind a single `RwLock`. Read-only
    /// lookups share the read lock; mutations take the write lock.
    struct InodeTable {
        next_ino: AtomicU64,
        inner: RwLock<InodeTableInner>,
    }

    impl InodeTable {
        fn new() -> Self {
            let mut ino_to_path = HashMap::new();
            let mut path_to_ino = HashMap::new();
            ino_to_path.insert(1, PathBuf::from(""));
            path_to_ino.insert(PathBuf::from(""), 1);

            Self {
                // inode 1 is the root directory.
                next_ino: AtomicU64::new(2),
                inner: RwLock::new(InodeTableInner {
                    ino_to_path,
                    path_to_ino,
                    lookup_count: HashMap::new(),
                    unlinked: HashSet::new(),
                }),
            }
        }

        fn get_path(&self, ino: u64) -> Option<PathBuf> {
            self.inner.read().unwrap().ino_to_path.get(&ino).cloned()
        }

        /// Return the inode for `path`, creating a new one if necessary.
        ///
        /// Each call increments the kernel lookup count for the returned
        /// inode.  The caller is responsible for only calling this when an
        /// inode is actually being handed to the kernel (lookup reply,
        /// create reply, readdir entry, etc.).
        fn get_or_create_inode(&self, path: &PathBuf) -> u64 {
            let mut inner = self.inner.write().unwrap();
            if let Some(&ino) = inner.path_to_ino.get(path) {
                *inner.lookup_count.entry(ino).or_insert(0) += 1;
                return ino;
            }
            let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
            inner.path_to_ino.insert(path.clone(), ino);
            inner.ino_to_path.insert(ino, path.clone());
            *inner.lookup_count.entry(ino).or_insert(0) += 1;
            ino
        }

        /// Unlink a path from the directory tree without dropping the inode.
        ///
        /// Removes the `path -> ino` mapping so the name is gone from the
        /// directory, but keeps the `ino -> path` entry alive so that open
        /// file descriptors can still resolve the inode to its backing
        /// storage path.  The inode is marked as unlinked; `forget()` will
        /// perform final cleanup when the kernel drops all references.
        fn unlink(&self, path: &PathBuf) {
            let mut inner = self.inner.write().unwrap();
            if let Some(ino) = inner.path_to_ino.remove(path) {
                inner.unlinked.insert(ino);
            }
        }

        /// Returns `true` if the inode has been unlinked from the directory
        /// tree but still has outstanding kernel references.
        fn is_unlinked(&self, ino: u64) -> bool {
            self.inner.read().unwrap().unlinked.contains(&ino)
        }

        /// Re-key an inode (and all child inodes for directory renames) from
        /// `old_path` to `new_path`.
        ///
        /// If the destination already has an inode mapping it is evicted
        /// (the old destination is being overwritten by POSIX rename
        /// semantics).
        fn rename(&self, old_path: &PathBuf, new_path: &PathBuf) {
            let mut inner = self.inner.write().unwrap();

            // The destination is being overwritten (POSIX rename semantics).
            // Remove from path_to_ino so lookup no longer finds it, but
            // keep ino_to_path alive for any open file descriptors.
            if let Some(dest_ino) = inner.path_to_ino.remove(new_path) {
                inner.unlinked.insert(dest_ino);
            }

            // Re-key the source itself.
            if let Some(ino) = inner.path_to_ino.remove(old_path) {
                inner.path_to_ino.insert(new_path.clone(), ino);
                inner.ino_to_path.insert(ino, new_path.clone());
            }

            // Re-key child paths (directory rename).
            let old_prefix = {
                let mut p = old_path.clone();
                p.push("");
                p
            };
            let children: Vec<(PathBuf, u64)> = inner
                .path_to_ino
                .iter()
                .filter(|(path, _)| path.starts_with(&old_prefix))
                .map(|(path, &ino)| (path.clone(), ino))
                .collect();
            for (child_path, ino) in children {
                if let Ok(suffix) = child_path.strip_prefix(old_path) {
                    let new_child = new_path.join(suffix);
                    inner.path_to_ino.remove(&child_path);
                    inner.path_to_ino.insert(new_child.clone(), ino);
                    inner.ino_to_path.insert(ino, new_child);
                }
            }
        }

        /// Decrement the kernel lookup count for `ino` by `nlookup`.
        /// When the count reaches zero the inode is evicted from the
        /// translation maps, freeing the memory.  The root inode (1) is
        /// never evicted.
        ///
        /// For unlinked inodes, `path_to_ino` was already removed by
        /// `unlink()` — only `ino_to_path` remains and is cleaned up here.
        /// For normal (non-unlinked) inodes, both maps are cleaned up.
        fn forget(&self, ino: u64, nlookup: u64) {
            if ino == 1 {
                // Root inode is permanent.
                return;
            }

            let mut inner = self.inner.write().unwrap();
            if let Some(count) = inner.lookup_count.get_mut(&ino) {
                *count = count.saturating_sub(nlookup);
                if *count == 0 {
                    inner.lookup_count.remove(&ino);
                    if inner.unlinked.remove(&ino) {
                        // Was unlinked — path_to_ino entry already removed;
                        // only ino_to_path remains.
                        inner.ino_to_path.remove(&ino);
                    } else {
                        // Normal forget — clean up both maps.
                        if let Some(path) = inner.ino_to_path.remove(&ino) {
                            inner.path_to_ino.remove(&path);
                        }
                    }
                }
            }
        }

        /// Remove all unlinked inodes that have no remaining kernel
        /// references (lookup count is zero or absent).
        ///
        /// Call this during overlay teardown to reclaim memory for inodes
        /// whose `forget` was never dispatched (e.g. after an unclean
        /// unmount or agent crash).
        fn purge_unlinked(&self) -> usize {
            let mut inner = self.inner.write().unwrap();
            let stale: Vec<u64> = inner
                .unlinked
                .iter()
                .filter(|&&ino| inner.lookup_count.get(&ino).is_none_or(|&count| count == 0))
                .copied()
                .collect();
            let purged = stale.len();
            for ino in stale {
                inner.unlinked.remove(&ino);
                inner.ino_to_path.remove(&ino);
                inner.lookup_count.remove(&ino);
            }
            purged
        }
    }

    /// Compute a stable, non-zero offset cookie for a directory entry name.
    ///
    /// FUSE uses the offset returned by `readdir` as a resumption cookie for
    /// chunked directory listings.  Using a hash of the entry name (instead of
    /// a volatile array index) keeps the cookie valid even when other entries
    /// are added or removed between calls.
    ///
    /// Uses FNV-1a (deterministic across process restarts, unlike
    /// `DefaultHasher` which is randomly seeded per process).
    pub(crate) fn dir_entry_cookie(name: &str) -> u64 {
        const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x00000100000001B3;
        let mut hash = FNV_OFFSET_BASIS;
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        // Ensure non-zero (offset 0 means "start from beginning" in FUSE)
        // and positive (avoid i64 sign issues in some FUSE implementations).
        (hash | 1) & 0x7FFF_FFFF_FFFF_FFFF
    }

    /// FUSE filesystem backed by an [`OverlayLayer`].
    pub struct PhantomFs {
        layer: RwLock<OverlayLayer>,
        agent_id: AgentId,
        inodes: InodeTable,
        /// Counter for allocating unique file handles.
        next_fh: AtomicU64,
        /// Open file descriptor table. Keyed by the file handle returned to
        /// the kernel via `open()` / `create()`.
        open_files: RwLock<HashMap<u64, OpenFile>>,
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

        fn readdir(
            &self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            mut reply: ReplyDirectory,
        ) {
            let Some(path) = self.inodes.get_path(ino.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            // Hold the read lock only for the directory listing, then release
            // before touching the inode table.
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

            // Resolve parent inode for ".." entry.
            let parent_ino = if ino.0 == 1 {
                1 // root's parent is itself
            } else {
                // Derive parent path and look up its inode.
                path.parent()
                    .map(|p| self.inodes.get_or_create_inode(&p.to_path_buf()))
                    .unwrap_or(1)
            };

            // Synthetic "." and ".." entries.
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

            // Sort entries by name for deterministic order across calls.
            all_entries.sort_by(|a, b| a.2.cmp(&b.2));

            // offset == 0 means start from the beginning.
            // offset != 0 means resume after the entry whose cookie matches.
            let start_idx = if offset == 0 {
                0
            } else {
                match all_entries
                    .iter()
                    .position(|(_, _, name)| dir_entry_cookie(name) == offset)
                {
                    Some(pos) => pos + 1,
                    None => {
                        // The entry for this cookie was deleted between paginated
                        // readdir calls.  Signal end-of-directory rather than
                        // restarting from index 0 (which would cause an infinite loop).
                        reply.ok();
                        return;
                    }
                }
            };

            for (_, (child_ino, ft, name)) in all_entries.iter().enumerate().skip(start_idx) {
                let cookie = dir_entry_cookie(name);
                if reply.add(INodeNo(*child_ino), cookie, *ft, name) {
                    break;
                }
            }
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
            let is_passthrough = layer.is_passthrough(&child_path);

            let target_path = if is_passthrough {
                layer.lower_dir().join(&child_path)
            } else {
                layer.upper_dir().join(&child_path)
            };

            if target_path.is_dir() {
                // Use remove_dir (not remove_dir_all) — POSIX rmdir fails on non-empty.
                match std::fs::remove_dir(&target_path) {
                    Ok(()) => {
                        if !is_passthrough {
                            // Add whiteout so the dir is hidden even if it exists in lower layer.
                            let _ = layer.delete_file(&child_path);
                        }
                        self.inodes.unlink(&child_path);
                        drop(layer);
                        reply.ok();
                    }
                    Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => {
                        reply.error(Errno::ENOTEMPTY);
                    }
                    Err(e) => {
                        warn!(error = %e, "rmdir failed");
                        reply.error(Errno::EIO);
                    }
                }
            } else {
                reply.error(Errno::ENOENT);
            }
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
}

#[cfg(target_os = "linux")]
pub use inner::PhantomFs;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::inner::dir_entry_cookie;

    #[test]
    fn dir_entry_cookie_is_deterministic() {
        let cookie1 = dir_entry_cookie("hello.txt");
        let cookie2 = dir_entry_cookie("hello.txt");
        assert_eq!(cookie1, cookie2);

        let cookie3 = dir_entry_cookie("world.txt");
        assert_ne!(cookie1, cookie3);
    }

    #[test]
    fn dir_entry_cookie_nonzero_and_positive() {
        for name in &[".", "..", "a", "hello.txt", "Cargo.toml"] {
            let cookie = dir_entry_cookie(name);
            assert_ne!(cookie, 0, "cookie for {name:?} must be non-zero");
            assert_eq!(
                cookie & 0x8000_0000_0000_0000,
                0,
                "cookie for {name:?} must be positive (top bit clear)"
            );
        }
    }

    #[test]
    fn dir_entry_cookie_dot_entries_differ() {
        let dot = dir_entry_cookie(".");
        let dotdot = dir_entry_cookie("..");
        assert_ne!(dot, dotdot);
    }

    #[test]
    fn dir_entry_cookie_known_value() {
        // Pin a known value to catch accidental algorithm changes.
        // FNV-1a of "test": 0xcbf29ce484222325 ^ 't' * prime ^ 'e' * prime ^ 's' * prime ^ 't' * prime
        let cookie = dir_entry_cookie("test");
        assert_eq!(cookie, dir_entry_cookie("test"));
        // Verify it's stable across runs by checking a hardcoded value.
        assert_eq!(cookie, 8783962037831871269);
    }
}
