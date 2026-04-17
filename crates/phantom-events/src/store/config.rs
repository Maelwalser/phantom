//! Configuration for [`SqliteEventStore`](super::SqliteEventStore).

/// Configuration for [`SqliteEventStore`](super::SqliteEventStore).
pub struct EventStoreConfig {
    /// Maximum number of connections in the SQLite pool.
    ///
    /// SQLite WAL mode allows concurrent readers with a single writer.
    /// Higher values help read-heavy workloads (multiple agents querying
    /// status concurrently).
    pub max_connections: u32,
}

impl Default for EventStoreConfig {
    fn default() -> Self {
        Self { max_connections: 2 }
    }
}
