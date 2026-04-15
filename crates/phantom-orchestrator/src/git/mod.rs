//! Git operations for Phantom — re-exported from `phantom-git`.
//!
//! This module provides backward-compatible re-exports of the `phantom-git`
//! crate's public API so that existing code within the orchestrator and
//! downstream crates can continue to use `phantom_orchestrator::git::*` paths.

pub use phantom_git::error::GitError;
pub use phantom_git::tree::{
    build_tree_from_oids, build_tree_with_blobs, create_blobs_from_content,
    create_blobs_from_overlay,
};
pub use phantom_git::{git_oid_to_oid, oid_to_git_oid, GitOps};
pub use phantom_git::test_support;
