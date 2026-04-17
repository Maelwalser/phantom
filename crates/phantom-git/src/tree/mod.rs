//! Git tree building from pre-created blobs, and blob creation from overlay
//! files or in-memory content.

#![allow(clippy::unreadable_literal)]

mod blobs;
mod build;

pub use blobs::{create_blobs_from_content, create_blobs_from_overlay};
pub use build::{build_tree_from_oids, build_tree_from_oids_with_deletions};
