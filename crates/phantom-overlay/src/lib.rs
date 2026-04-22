// FUSE filesystem requires libc calls (getuid/getgid) for file ownership.
#![allow(unsafe_code)]
//! `phantom-overlay` — FUSE overlay filesystem for per-agent isolation.
//!
//! Each agent gets a copy-on-write overlay: reads fall through to trunk,
//! writes go to a per-agent upper layer.

pub mod error;
mod exclusion;
pub mod layer;
pub mod manager;
pub mod trunk_view;
pub mod types;
mod whiteout;

// FUSE support is Linux-only. Both the `fuse` feature and the `linux` target
// are required — gate at the module-declaration site so the submodules do
// not need inner cfg wrappers of their own.
#[cfg(all(feature = "fuse", target_os = "linux"))]
pub mod fuse_fs;
#[cfg(all(feature = "fuse", target_os = "linux"))]
mod inode_table;

pub use error::OverlayError;
pub use layer::OverlayLayer;
pub use layer::list_modified_files_in_upper;
pub use manager::{MountHandle, OverlayManager};
pub use trunk_view::TrunkView;
pub use types::{DirEntry, FileType};

#[cfg(all(feature = "fuse", target_os = "linux"))]
pub use fuse_fs::{FsConfig, PhantomFs};
