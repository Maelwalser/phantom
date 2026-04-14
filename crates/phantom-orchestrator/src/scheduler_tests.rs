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
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
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
