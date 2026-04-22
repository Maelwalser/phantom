//! `phantom-orchestrator` — coordination layer for Phantom.
//!
//! Manages task scheduling, changeset materialization to trunk, ripple
//! notifications to active agents, and low-level git operations.

pub mod error;
pub mod git;
pub mod impact;
pub mod live_rebase;
pub mod materialization_service;
pub mod materializer;
pub(crate) mod ops;
pub mod pending_notifications;
pub mod ripple;
pub mod submit_service;
#[cfg(test)]
pub(crate) mod test_support;
pub mod trunk_update;
