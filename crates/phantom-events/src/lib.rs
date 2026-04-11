//! `phantom-events` — SQLite-backed append-only event store.
//!
//! Implements [`phantom_core::EventStore`] using SQLite in WAL mode for
//! concurrent readers with a single writer.

pub mod projection;
pub mod query;
pub mod replay;
pub mod store;
