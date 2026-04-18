//! FUSE filesystem adapter for the copy-on-write overlay.
//!
//! [`PhantomFs`] wraps an [`OverlayLayer`](crate::layer::OverlayLayer) and
//! exposes it as a FUSE filesystem via the `fuser` crate. Compiled only on
//! Linux with the `fuse` feature — the module declaration in `lib.rs`
//! already enforces that gate.
//!
//! The implementation is split across submodules by responsibility:
//!
//! - `config` — [`FsConfig`]: UID/GID/permission projection
//! - `attr` — metadata → `fuser::FileAttr` conversion
//! - `handles` — open file / directory handle types
//! - `filesystem` — [`PhantomFs`] struct and `fuser::Filesystem` trait impl

mod attr;
mod config;
mod filesystem;
mod handles;

pub use config::FsConfig;
pub use filesystem::PhantomFs;
