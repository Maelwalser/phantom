//! FUSE filesystem adapter for the copy-on-write overlay.
//!
//! `PhantomFs` wraps an `OverlayLayer` and exposes it as a FUSE
//! filesystem via the `fuser` crate. This module is only compiled on Linux.

#[cfg(target_os = "linux")]
mod inner {
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Mutex;
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

    /// TTL for attribute and entry caching (1 second).
    const TTL: Duration = Duration::from_secs(1);

    /// Bidirectional map between inode numbers and filesystem paths.
    ///
    /// Tracks kernel lookup counts so that inodes can be evicted via the
    /// FUSE `forget` callback, preventing unbounded growth when large
    /// directory trees are traversed.
    struct InodeTable {
        next_ino: AtomicU64,
        ino_to_path: Mutex<HashMap<u64, PathBuf>>,
        path_to_ino: Mutex<HashMap<PathBuf, u64>>,
        /// Kernel-side lookup reference count per inode.  Incremented on
        /// every `lookup`, `create`, `mkdir`, and `readdir` reply that
        /// hands an inode to the kernel; decremented by `forget`.
        lookup_count: Mutex<HashMap<u64, u64>>,
        /// Inodes that have been unlinked from the directory tree but still
        /// have a non-zero kernel lookup count.  The `ino_to_path` entry is
        /// kept alive so that open file descriptors can still resolve the
        /// inode.  `forget()` performs final cleanup when the count drops
        /// to zero.
        unlinked: Mutex<HashSet<u64>>,
    }

    impl InodeTable {
        fn new() -> Self {
            let table = Self {
                // inode 1 is the root directory.
                next_ino: AtomicU64::new(2),
                ino_to_path: Mutex::new(HashMap::new()),
                path_to_ino: Mutex::new(HashMap::new()),
                lookup_count: Mutex::new(HashMap::new()),
                unlinked: Mutex::new(HashSet::new()),
            };
            // Root directory is inode 1.
            table
                .ino_to_path
                .lock()
                .unwrap()
                .insert(1, PathBuf::from(""));
            table
                .path_to_ino
                .lock()
                .unwrap()
                .insert(PathBuf::from(""), 1);
            table
        }

        fn get_path(&self, ino: u64) -> Option<PathBuf> {
            self.ino_to_path.lock().unwrap().get(&ino).cloned()
        }

        /// Return the inode for `path`, creating a new one if necessary.
        ///
        /// Each call increments the kernel lookup count for the returned
        /// inode.  The caller is responsible for only calling this when an
        /// inode is actually being handed to the kernel (lookup reply,
        /// create reply, readdir entry, etc.).
        fn get_or_create_inode(&self, path: &PathBuf) -> u64 {
            let mut p2i = self.path_to_ino.lock().unwrap();
            if let Some(&ino) = p2i.get(path) {
                *self.lookup_count.lock().unwrap().entry(ino).or_insert(0) += 1;
                return ino;
            }
            let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
            p2i.insert(path.clone(), ino);
            self.ino_to_path.lock().unwrap().insert(ino, path.clone());
            *self.lookup_count.lock().unwrap().entry(ino).or_insert(0) += 1;
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
            let mut p2i = self.path_to_ino.lock().unwrap();
            if let Some(ino) = p2i.remove(path) {
                self.unlinked.lock().unwrap().insert(ino);
            }
        }

        /// Returns `true` if the inode has been unlinked from the directory
        /// tree but still has outstanding kernel references.
        fn is_unlinked(&self, ino: u64) -> bool {
            self.unlinked.lock().unwrap().contains(&ino)
        }

        /// Re-key an inode (and all child inodes for directory renames) from
        /// `old_path` to `new_path`.
        ///
        /// If the destination already has an inode mapping it is evicted
        /// (the old destination is being overwritten by POSIX rename
        /// semantics).
        fn rename(&self, old_path: &PathBuf, new_path: &PathBuf) {
            let mut p2i = self.path_to_ino.lock().unwrap();
            let mut i2p = self.ino_to_path.lock().unwrap();

            // The destination is being overwritten (POSIX rename semantics).
            // Remove from path_to_ino so lookup no longer finds it, but
            // keep ino_to_path alive for any open file descriptors.
            if let Some(dest_ino) = p2i.remove(new_path) {
                self.unlinked.lock().unwrap().insert(dest_ino);
            }

            // Re-key the source itself.
            if let Some(ino) = p2i.remove(old_path) {
                p2i.insert(new_path.clone(), ino);
                i2p.insert(ino, new_path.clone());
            }

            // Re-key child paths (directory rename).
            let old_prefix = {
                let mut p = old_path.clone();
                p.push("");
                p
            };
            let children: Vec<(PathBuf, u64)> = p2i
                .iter()
                .filter(|(path, _)| path.starts_with(&old_prefix))
                .map(|(path, &ino)| (path.clone(), ino))
                .collect();
            for (child_path, ino) in children {
                if let Ok(suffix) = child_path.strip_prefix(old_path) {
                    let new_child = new_path.join(suffix);
                    p2i.remove(&child_path);
                    p2i.insert(new_child.clone(), ino);
                    i2p.insert(ino, new_child);
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

            let mut counts = self.lookup_count.lock().unwrap();
            if let Some(count) = counts.get_mut(&ino) {
                *count = count.saturating_sub(nlookup);
                if *count == 0 {
                    counts.remove(&ino);
                    let mut unlinked = self.unlinked.lock().unwrap();
                    if unlinked.remove(&ino) {
                        // Was unlinked — path_to_ino entry already removed;
                        // only ino_to_path remains.
                        self.ino_to_path.lock().unwrap().remove(&ino);
                    } else {
                        // Normal forget — clean up both maps.
                        let mut i2p = self.ino_to_path.lock().unwrap();
                        if let Some(path) = i2p.remove(&ino) {
                            self.path_to_ino.lock().unwrap().remove(&path);
                        }
                    }
                }
            }
        }
    }

    /// FUSE filesystem backed by an [`OverlayLayer`].
    pub struct PhantomFs {
        layer: Mutex<OverlayLayer>,
        agent_id: AgentId,
        inodes: InodeTable,
    }

    impl PhantomFs {
        /// Create a new FUSE filesystem for the given agent.
        pub fn new(layer: OverlayLayer, agent_id: AgentId) -> Self {
            Self {
                layer: Mutex::new(layer),
                agent_id,
                inodes: InodeTable::new(),
            }
        }

        /// Return the agent ID this filesystem belongs to.
        #[must_use]
        pub fn agent_id(&self) -> &AgentId {
            &self.agent_id
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
            let layer = self.layer.lock().unwrap();

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
                let layer = self.layer.lock().unwrap();
                match layer.getattr(&path) {
                    Ok(meta) => reply.attr(&TTL, &metadata_to_attr(ino.0, &meta)),
                    Err(_) => reply.attr(&TTL, &default_dir_attr(ino.0)),
                }
                return;
            }

            let layer = self.layer.lock().unwrap();
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

            let layer = self.layer.lock().unwrap();
            let entries = match layer.read_dir(&path) {
                Ok(e) => e,
                Err(_) => {
                    reply.error(Errno::ENOENT);
                    return;
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
                    crate::layer::FileType::File => FileType::RegularFile,
                    crate::layer::FileType::Directory => FileType::Directory,
                    crate::layer::FileType::Symlink => FileType::Symlink,
                };
                all_entries.push((child_ino, ft, entry.name.to_string_lossy().into_owned()));
            }

            for (i, (child_ino, ft, name)) in all_entries.iter().enumerate().skip(offset as usize) {
                if reply.add(INodeNo(*child_ino), (i + 1) as u64, *ft, name) {
                    break;
                }
            }
            reply.ok();
        }

        fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
            if self.inodes.get_path(ino.0).is_some() {
                reply.opened(FileHandle(0), FopenFlags::empty());
            } else {
                reply.error(Errno::ENOENT);
            }
        }

        fn read(
            &self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            size: u32,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: ReplyData,
        ) {
            let Some(path) = self.inodes.get_path(ino.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let layer = self.layer.lock().unwrap();
            match layer.read_file(&path) {
                Ok(data) => {
                    let start = offset as usize;
                    if start >= data.len() {
                        reply.data(&[]);
                    } else {
                        let end = (start + size as usize).min(data.len());
                        reply.data(&data[start..end]);
                    }
                }
                Err(_) => reply.error(Errno::ENOENT),
            }
        }

        fn write(
            &self,
            _req: &Request,
            ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            data: &[u8],
            _write_flags: WriteFlags,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: ReplyWrite,
        ) {
            let Some(path) = self.inodes.get_path(ino.0) else {
                reply.error(Errno::ENOENT);
                return;
            };

            let mut layer = self.layer.lock().unwrap();

            // Read existing content (may not exist yet).
            let mut content = layer.read_file(&path).unwrap_or_default();

            // Splice in new data at offset.
            let start = offset as usize;
            if start > content.len() {
                content.resize(start, 0);
            }
            let end = start + data.len();
            if end > content.len() {
                content.resize(end, 0);
            }
            content[start..end].copy_from_slice(data);

            match layer.write_file(&path, &content) {
                Ok(()) => {
                    layer.remove_whiteout(&path);
                    reply.written(data.len() as u32);
                }
                Err(e) => {
                    warn!(error = %e, "write failed");
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
            let mut layer = self.layer.lock().unwrap();

            match layer.write_file(&child_path, &[]) {
                Ok(()) => {
                    layer.remove_whiteout(&child_path);
                    let ino = self.inodes.get_or_create_inode(&child_path);
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
                        FileHandle(0),
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
            let mut layer = self.layer.lock().unwrap();

            match layer.delete_file(&child_path) {
                Ok(()) => {
                    drop(layer);
                    self.inodes.unlink(&child_path);
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

            // Handle chmod.
            if let Some(new_mode) = mode {
                let mut layer = self.layer.lock().unwrap();
                if let Err(e) = layer.set_permissions(&path, new_mode) {
                    warn!(error = %e, "setattr chmod failed");
                    reply.error(Errno::EIO);
                    return;
                }
            }

            // Handle truncate (size = 0 or explicit size).
            if let Some(new_size) = size {
                let mut layer = self.layer.lock().unwrap();
                if let Err(e) = layer.truncate_file(&path, new_size) {
                    warn!(error = %e, "setattr truncate failed");
                    reply.error(Errno::EIO);
                    return;
                }
            }

            let layer = self.layer.lock().unwrap();
            match layer.getattr(&path) {
                Ok(meta) => reply.attr(&TTL, &metadata_to_attr(ino.0, &meta)),
                Err(_) => reply.error(Errno::ENOENT),
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
            let mut layer = self.layer.lock().unwrap();

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

            let mut layer = self.layer.lock().unwrap();

            // RENAME_NOREPLACE: fail if destination already exists.
            if flags.contains(RenameFlags::RENAME_NOREPLACE) && layer.exists(&new_path) {
                reply.error(Errno::EEXIST);
                return;
            }

            match layer.rename_file(&old_path, &new_path) {
                Ok(()) => {
                    drop(layer);
                    self.inodes.rename(&old_path, &new_path);
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
            let mut layer = self.layer.lock().unwrap();
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
                        drop(layer);
                        self.inodes.unlink(&child_path);
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
    }
}

#[cfg(target_os = "linux")]
pub use inner::PhantomFs;
