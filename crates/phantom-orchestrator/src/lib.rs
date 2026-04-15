//! `phantom-orchestrator` — coordination layer for Phantom.
//!
//! Manages task scheduling, changeset materialization to trunk, ripple
//! notifications to active agents, and low-level git operations.

pub mod error;
pub mod git;
pub mod live_rebase;
pub mod materialization_service;
pub mod materializer;
pub(crate) mod ops;
pub mod ripple;
#[cfg(test)]
pub(crate) mod test_support;
pub mod scheduler;
pub mod submit_service;
pub mod trunk_update;
