//! `phantom-overlay` — FUSE overlay filesystem for per-agent isolation.
//!
//! Each agent gets a copy-on-write overlay: reads fall through to trunk,
//! writes go to a per-agent upper layer.

pub mod error;
#[cfg(feature = "fuse")]
pub mod fuse_fs;
pub mod layer;
pub mod manager;
pub mod trunk_view;
pub mod types;
mod whiteout;

pub use error::OverlayError;
pub use layer::OverlayLayer;
pub use manager::{MountHandle, OverlayManager};
pub use trunk_view::TrunkView;
pub use types::{DirEntry, FileType};

#[cfg(feature = "fuse")]
pub use fuse_fs::PhantomFs;
