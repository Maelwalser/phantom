//! FUSE filesystem adapter for the copy-on-write overlay.
//!
//! `PhantomFs` wraps an `OverlayLayer` and exposes it as a FUSE
//! filesystem via the `fuser` crate. This module is only compiled on Linux.

#[cfg(target_os = "linux")]
mod inner {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use fuser::{
        BsdFileFlags, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation,
        INodeNo, LockOwner, OpenFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
        ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, WriteFlags,
    };
    use phantom_core::AgentId;
    use tracing::{debug, warn};

    use crate::layer::OverlayLayer;

    /// TTL for attribute and entry caching (1 second).
    const TTL: Duration = Duration::from_secs(1);

    /// Bidirectional map between inode numbers and filesystem paths.
    struct InodeTable {
        next_ino: AtomicU64,
        ino_to_path: Mutex<HashMap<u64, PathBuf>>,
        path_to_ino: Mutex<HashMap<PathBuf, u64>>,
    }

    impl InodeTable {
        fn new() -> Self {
            let table = Self {
                // inode 1 is the root directory.
                next_ino: AtomicU64::new(2),
                ino_to_path: Mutex::new(HashMap::new()),
                path_to_ino: Mutex::new(HashMap::new()),
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

        fn get_or_create_inode(&self, path: &PathBuf) -> u64 {
            let mut p2i = self.path_to_ino.lock().unwrap();
            if let Some(&ino) = p2i.get(path) {
                return ino;
            }
            let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
            p2i.insert(path.clone(), ino);
            self.ino_to_path.lock().unwrap().insert(ino, path.clone());
            ino
        }

        fn remove(&self, path: &PathBuf) {
            let mut p2i = self.path_to_ino.lock().unwrap();
            if let Some(ino) = p2i.remove(path) {
                self.ino_to_path.lock().unwrap().remove(&ino);
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
            perm: 0o755,
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
                Ok(meta) => reply.attr(&TTL, &metadata_to_attr(ino.0, &meta)),
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
                    self.inodes.remove(&child_path);
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
            _mode: Option<u32>,
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

            // Handle truncate (size = 0 or explicit size).
            if let Some(new_size) = size {
                let mut layer = self.layer.lock().unwrap();
                let mut content = layer.read_file(&path).unwrap_or_default();
                content.resize(new_size as usize, 0);
                if let Err(e) = layer.write_file(&path, &content) {
                    warn!(error = %e, "setattr truncate failed");
                    reply.error(Errno::EIO);
                    return;
                }
                layer.remove_whiteout(&path);
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
                        self.inodes.remove(&child_path);
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
