//! `phantom-git` — git operations for Phantom, built on `git2`.
//!
//! Provides [`GitOps`] — a wrapper around a `git2::Repository` — and
//! lossless conversions between [`phantom_core::GitOid`] and [`git2::Oid`].
//!
//! This crate depends only on `phantom-core` and `git2`, keeping git
//! operations decoupled from the event store, semantic analysis, and overlay
//! filesystem.

pub mod error;
pub mod oid;
pub mod ops;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_support;
pub mod tree;

mod fs_walk;
mod merge;

pub use oid::{git_oid_to_oid, oid_to_git_oid};
pub use ops::GitOps;
pub use tree::{
    build_tree_from_oids, build_tree_from_oids_with_deletions, create_blobs_from_content,
    create_blobs_from_overlay,
};
