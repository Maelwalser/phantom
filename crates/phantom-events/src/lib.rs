//! `phantom-events` — SQLite-backed append-only event store.
//!
//! Implements [`phantom_core::EventStore`] using SQLite in WAL mode via sqlx
//! for async concurrent access. Provides advanced querying, replay for
//! rollback support, and projection to derive current state from the event log.

pub mod error;
mod kind_pattern;
pub mod projection;
pub mod query;
pub mod replay;
mod schema;
pub mod snapshot;
pub mod store;

#[cfg(test)]
mod schema_tests;

pub use error::EventStoreError;
pub use projection::Projection;
pub use query::{EventQuery, QueryOrder};
pub use replay::{OrphanFence, ReplayEngine};
pub use snapshot::SnapshotManager;
pub use store::{EventStoreConfig, SqliteEventStore};
