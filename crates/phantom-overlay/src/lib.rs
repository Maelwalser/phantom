//! `phantom-overlay` — FUSE overlay filesystem for per-agent isolation.
//!
//! Each agent gets a copy-on-write overlay: reads fall through to trunk,
//! writes go to a per-agent upper layer.

pub mod fuse_fs;
pub mod layer;
pub mod manager;
pub mod trunk_view;
