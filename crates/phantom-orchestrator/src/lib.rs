//! `phantom-orchestrator` — coordination layer for Phantom.
//!
//! Manages task scheduling, changeset materialization to trunk, ripple
//! notifications to active agents, and low-level git operations.

pub mod error;
pub mod git;
pub mod live_rebase;
pub mod materializer;
pub mod ripple;
pub mod scheduler;
