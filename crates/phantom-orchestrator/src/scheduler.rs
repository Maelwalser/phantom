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
mod tests {
    use super::*;
    use phantom_core::changeset::ChangesetStatus;
    use phantom_core::id::{AgentId, GitOid};

    fn make_changeset(id: &str) -> Changeset {
        Changeset {
            id: ChangesetId(id.into()),
            agent_id: AgentId("agent-test".into()),
            task: format!("task for {id}"),
            base_commit: GitOid::zero(),
            files_touched: vec![],
            operations: vec![],
            test_result: None,
            created_at: chrono::Utc::now(),
            status: ChangesetStatus::Submitted,
            interactive_session_active: false,
        }
    }

    #[test]
    fn fifo_order() {
        let mut sched = Scheduler::new();
        sched.enqueue(make_changeset("cs-1"));
        sched.enqueue(make_changeset("cs-2"));
        sched.enqueue(make_changeset("cs-3"));

        assert_eq!(sched.dequeue().unwrap().id, ChangesetId("cs-1".into()));
        assert_eq!(sched.dequeue().unwrap().id, ChangesetId("cs-2".into()));
        assert_eq!(sched.dequeue().unwrap().id, ChangesetId("cs-3".into()));
        assert!(sched.dequeue().is_none());
    }

    #[test]
    fn remove_from_middle() {
        let mut sched = Scheduler::new();
        sched.enqueue(make_changeset("cs-1"));
        sched.enqueue(make_changeset("cs-2"));
        sched.enqueue(make_changeset("cs-3"));

        let removed = sched.remove(&ChangesetId("cs-2".into()));
        assert_eq!(removed.unwrap().id, ChangesetId("cs-2".into()));

        assert_eq!(sched.len(), 2);
        assert_eq!(sched.dequeue().unwrap().id, ChangesetId("cs-1".into()));
        assert_eq!(sched.dequeue().unwrap().id, ChangesetId("cs-3".into()));
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut sched = Scheduler::new();
        sched.enqueue(make_changeset("cs-1"));
        assert!(sched.remove(&ChangesetId("cs-999".into())).is_none());
        assert_eq!(sched.len(), 1);
    }

    #[test]
    fn pending_shows_all_without_consuming() {
        let mut sched = Scheduler::new();
        sched.enqueue(make_changeset("cs-1"));
        sched.enqueue(make_changeset("cs-2"));

        let pending: Vec<_> = sched.pending().collect();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, ChangesetId("cs-1".into()));
        assert_eq!(pending[1].id, ChangesetId("cs-2".into()));

        // Queue is not consumed
        assert_eq!(sched.len(), 2);
    }

    #[test]
    fn empty_scheduler() {
        let mut sched = Scheduler::new();
        assert!(sched.is_empty());
        assert_eq!(sched.len(), 0);
        assert!(sched.dequeue().is_none());
        assert_eq!(sched.pending().count(), 0);
    }

    #[test]
    fn default_creates_empty() {
        let sched = Scheduler::default();
        assert!(sched.is_empty());
    }
}
