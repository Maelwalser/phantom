//! Snapshot auto-save policy.
//!
//! Decouples the "when do we persist a snapshot?" question from both the
//! I/O layer ([`super::repository`]) and the projection building logic
//! ([`super::SnapshotManager`]). Today the policy is a simple event-count
//! threshold; future policies could factor in time elapsed, snapshot age,
//! or per-changeset activity without touching callers.

/// Default number of tail events that triggers a new snapshot write.
const DEFAULT_SNAPSHOT_INTERVAL: u64 = 100;

/// Configurable policy for deciding when to persist a projection snapshot.
#[derive(Debug, Clone, Copy)]
pub(super) struct SnapshotPolicy {
    interval: u64,
}

impl SnapshotPolicy {
    /// Create a policy with the default interval.
    pub(super) fn new() -> Self {
        Self {
            interval: DEFAULT_SNAPSHOT_INTERVAL,
        }
    }

    /// Create a policy with a custom interval. Used by tests.
    #[cfg(test)]
    pub(super) fn with_interval(interval: u64) -> Self {
        Self { interval }
    }

    /// Should we persist a snapshot after replaying `tail_len` events?
    pub(super) fn should_snapshot(self, tail_len: u64) -> bool {
        tail_len >= self.interval
    }
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self::new()
    }
}
