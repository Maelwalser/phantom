//! Agent session lifecycle: PTY management, CLI adapters, context files, and
//! post-session automation (submit + materialize flow).
//!
//! This crate extracts non-CLI logic from `phantom-cli` so it can be reused
//! independently of the clap command layer.

pub mod adapter;
pub mod context_file;
pub mod post_session;
pub mod pty;
