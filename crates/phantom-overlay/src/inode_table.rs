//! Bidirectional inode-to-path translation table for the FUSE layer.
//!
//! [`InodeTable`] tracks kernel lookup counts so that inodes can be evicted
//! via the FUSE `forget` callback, preventing unbounded growth when large
//! directory trees are traversed.

#[cfg(target_os = "linux")]
mod inner {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::types::reparent_children;

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
    pub(crate) struct InodeTable {
        next_ino: AtomicU64,
        inner: RwLock<InodeTableInner>,
    }

    impl InodeTable {
        pub(crate) fn new() -> Self {
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

        pub(crate) fn get_path(&self, ino: u64) -> Option<PathBuf> {
            self.inner.read().unwrap().ino_to_path.get(&ino).cloned()
        }

        /// Return the inode for `path`, creating a new one if necessary.
        ///
        /// Each call increments the kernel lookup count for the returned
        /// inode.  The caller is responsible for only calling this when an
        /// inode is actually being handed to the kernel (lookup reply,
        /// create reply, readdir entry, etc.).
        ///
        /// NOTE: This takes a write lock unconditionally because the
        /// lookup_count must be incremented atomically with the lookup.
        /// A read-then-write approach would introduce a TOCTOU race.
        /// If profiling shows this is a bottleneck under high concurrency
        /// (e.g. LSP indexing), consider migrating to `DashMap` or
        /// per-shard locking.
        pub(crate) fn get_or_create_inode(&self, path: &PathBuf) -> u64 {
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
        pub(crate) fn unlink(&self, path: &PathBuf) {
            let mut inner = self.inner.write().unwrap();
            if let Some(ino) = inner.path_to_ino.remove(path) {
                inner.unlinked.insert(ino);
            }
        }

        /// Returns `true` if the inode has been unlinked from the directory
        /// tree but still has outstanding kernel references.
        pub(crate) fn is_unlinked(&self, ino: u64) -> bool {
            self.inner.read().unwrap().unlinked.contains(&ino)
        }

        /// Re-key an inode (and all child inodes for directory renames) from
        /// `old_path` to `new_path`.
        ///
        /// If the destination already has an inode mapping it is evicted
        /// (the old destination is being overwritten by POSIX rename
        /// semantics).
        pub(crate) fn rename(&self, old_path: &PathBuf, new_path: &PathBuf) {
            // POSIX: renaming a path to itself is a no-op.
            if old_path == new_path {
                return;
            }

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
            let keys: Vec<PathBuf> = inner.path_to_ino.keys().cloned().collect();
            let children = reparent_children(keys.iter(), old_path, new_path);
            for (old_child, new_child) in children {
                if let Some(ino) = inner.path_to_ino.remove(&old_child) {
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
        pub(crate) fn forget(&self, ino: u64, nlookup: u64) {
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
        pub(crate) fn purge_unlinked(&self) -> usize {
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

    #[cfg(test)]
    #[path = "inode_table_tests.rs"]
    mod tests;
}

#[cfg(target_os = "linux")]
pub(crate) use inner::InodeTable;
