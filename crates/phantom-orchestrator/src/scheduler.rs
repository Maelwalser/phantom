//! Task queue for changeset scheduling.
//!
//! [`Scheduler`] maintains a FIFO queue of changesets awaiting materialization.
//! Priority scheduling can be layered on top later; the current implementation
//! is intentionally simple.

use std::collections::VecDeque;

use phantom_core::changeset::Changeset;
use phantom_core::id::ChangesetId;

/// FIFO task queue for pending changesets.
#[derive(Debug)]
pub struct Scheduler {
    queue: VecDeque<Changeset>,
}

impl Scheduler {
    /// Create an empty scheduler.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Push a changeset to the back of the queue.
    pub fn enqueue(&mut self, changeset: Changeset) {
        self.queue.push_back(changeset);
    }

    /// Pop the next changeset from the front of the queue.
    pub fn dequeue(&mut self) -> Option<Changeset> {
        self.queue.pop_front()
    }

    /// Iterate over all pending changesets without consuming them.
    pub fn pending(&self) -> impl Iterator<Item = &Changeset> {
        self.queue.iter()
    }

    /// Remove a specific changeset by ID, returning it if found.
    pub fn remove(&mut self, id: &ChangesetId) -> Option<Changeset> {
        let pos = self.queue.iter().position(|cs| cs.id == *id)?;
        self.queue.remove(pos)
    }

    /// Return the number of pending changesets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Return `true` if no changesets are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "scheduler_tests.rs"]
mod tests;
