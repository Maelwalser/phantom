//! Derive current codebase state from events.
//!
//! [`Projection`] iterates an event stream and builds a map of changeset
//! states, enabling queries like "which agents are active?" and "which
//! changesets are pending materialization?".

use std::collections::HashMap;

use phantom_core::changeset::{Changeset, ChangesetStatus};
use phantom_core::event::{Event, EventKind};
use phantom_core::id::{AgentId, ChangesetId, GitOid};

/// Projected state derived from an event stream.
pub struct Projection {
    changesets: HashMap<ChangesetId, Changeset>,
}

impl Projection {
    /// Build a projection by replaying events in order.
    ///
    /// Each event updates the corresponding changeset record:
    /// - `TaskCreated` → new changeset with `InProgress` status
    /// - `ChangesetSubmitted` → status becomes `Submitted`, operations stored
    /// - `ChangesetMaterialized` → status becomes `Materialized`
    /// - `ChangesetConflicted` → status becomes `Conflicted`
    /// - `ChangesetDropped` → status becomes `Dropped`
    /// - `TestsRun` → test results updated
    #[must_use]
    pub fn from_events(events: &[Event]) -> Self {
        let mut changesets: HashMap<ChangesetId, Changeset> = HashMap::new();

        for event in events {
            let cs = changesets
                .entry(event.changeset_id.clone())
                .or_insert_with(|| new_changeset(&event.changeset_id, &event.agent_id));

            match &event.kind {
                EventKind::TaskCreated { base_commit, task } => {
                    cs.status = ChangesetStatus::InProgress;
                    cs.base_commit = *base_commit;
                    cs.task = task.clone();
                    cs.created_at = event.timestamp;
                }
                EventKind::ChangesetSubmitted { operations } => {
                    cs.status = ChangesetStatus::Submitted;
                    cs.operations = operations.clone();
                    // Extract touched file paths from semantic operations so that
                    // the materializer knows which files to merge.
                    for op in operations {
                        let path = match op {
                            phantom_core::changeset::SemanticOperation::AddSymbol {
                                file, ..
                            }
                            | phantom_core::changeset::SemanticOperation::ModifySymbol {
                                file,
                                ..
                            }
                            | phantom_core::changeset::SemanticOperation::DeleteSymbol {
                                file,
                                ..
                            } => Some(file.clone()),
                            phantom_core::changeset::SemanticOperation::AddFile { path } => {
                                Some(path.clone())
                            }
                            phantom_core::changeset::SemanticOperation::DeleteFile { path } => {
                                Some(path.clone())
                            }
                            phantom_core::changeset::SemanticOperation::RawDiff {
                                path, ..
                            } => Some(path.clone()),
                        };
                        if let Some(p) = path
                            && !cs.files_touched.contains(&p)
                        {
                            cs.files_touched.push(p);
                        }
                    }
                }
                EventKind::ChangesetMaterialized { .. } => {
                    cs.status = ChangesetStatus::Materialized;
                }
                EventKind::ChangesetConflicted { .. } => {
                    cs.status = ChangesetStatus::Conflicted;
                }
                EventKind::ChangesetDropped { .. } => {
                    cs.status = ChangesetStatus::Dropped;
                }
                EventKind::TestsRun(result) => {
                    cs.test_result = Some(*result);
                }
                EventKind::FileWritten { path, .. } => {
                    if !cs.files_touched.contains(path) {
                        cs.files_touched.push(path.clone());
                    }
                }
                EventKind::FileDeleted { path } => {
                    if !cs.files_touched.contains(path) {
                        cs.files_touched.push(path.clone());
                    }
                }
                EventKind::AgentLaunched { pid, .. } => {
                    cs.agent_pid = Some(*pid);
                    cs.agent_launched_at = Some(event.timestamp);
                }
                EventKind::AgentCompleted { exit_code, .. } => {
                    cs.agent_exit_code = *exit_code;
                    cs.agent_completed_at = Some(event.timestamp);
                }
                // Other event kinds don't affect changeset state.
                _ => {}
            }
        }

        Self { changesets }
    }

    /// Look up a changeset by ID.
    #[must_use]
    pub fn changeset(&self, id: &ChangesetId) -> Option<&Changeset> {
        self.changesets.get(id)
    }

    /// Return agents that have at least one `InProgress` changeset.
    #[must_use]
    pub fn active_agents(&self) -> Vec<AgentId> {
        let mut agents: Vec<AgentId> = self
            .changesets
            .values()
            .filter(|cs| cs.status == ChangesetStatus::InProgress)
            .map(|cs| cs.agent_id.clone())
            .collect();
        agents.sort_by(|a, b| a.0.cmp(&b.0));
        agents.dedup();
        agents
    }

    /// Find the most recently submitted changeset for a given agent.
    ///
    /// Returns the changeset with `Submitted` status whose `created_at` is
    /// latest among all submitted changesets belonging to `agent_id`.
    /// Returns `None` if no submitted changeset exists for that agent.
    #[must_use]
    pub fn latest_submitted_changeset(&self, agent_id: &AgentId) -> Option<&Changeset> {
        self.changesets
            .values()
            .filter(|cs| cs.agent_id == *agent_id && cs.status == ChangesetStatus::Submitted)
            .max_by_key(|cs| cs.created_at)
    }

    /// Find the most recently conflicted changeset for a given agent.
    ///
    /// Returns the changeset with `Conflicted` status whose `created_at` is
    /// latest among all conflicted changesets belonging to `agent_id`.
    /// Returns `None` if no conflicted changeset exists for that agent.
    #[must_use]
    pub fn latest_conflicted_changeset(&self, agent_id: &AgentId) -> Option<&Changeset> {
        self.changesets
            .values()
            .filter(|cs| cs.agent_id == *agent_id && cs.status == ChangesetStatus::Conflicted)
            .max_by_key(|cs| cs.created_at)
    }

    /// Return changesets with `Submitted` status.
    #[must_use]
    pub fn pending_changesets(&self) -> Vec<&Changeset> {
        let mut pending: Vec<&Changeset> = self
            .changesets
            .values()
            .filter(|cs| cs.status == ChangesetStatus::Submitted)
            .collect();
        pending.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        pending
    }
}

/// Create a default changeset shell for projection bookkeeping.
fn new_changeset(id: &ChangesetId, agent_id: &AgentId) -> Changeset {
    Changeset {
        id: id.clone(),
        agent_id: agent_id.clone(),
        task: String::new(),
        base_commit: GitOid::zero(),
        files_touched: Vec::new(),
        operations: Vec::new(),
        test_result: None,
        created_at: chrono::Utc::now(),
        status: ChangesetStatus::InProgress,
        agent_pid: None,
        agent_launched_at: None,
        agent_completed_at: None,
        agent_exit_code: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phantom_core::event::{Event, EventKind};
    use phantom_core::id::EventId;

    fn make_event(
        id: u64,
        changeset: &str,
        agent: &str,
        kind: EventKind,
        timestamp: chrono::DateTime<chrono::Utc>,
    ) -> Event {
        Event {
            id: EventId(id),
            timestamp,
            changeset_id: ChangesetId(changeset.into()),
            agent_id: AgentId(agent.into()),
            kind,
        }
    }

    #[test]
    fn latest_submitted_changeset_returns_none_when_no_changesets() {
        let projection = Projection::from_events(&[]);
        assert!(
            projection
                .latest_submitted_changeset(&AgentId("agent-a".into()))
                .is_none()
        );
    }

    #[test]
    fn latest_submitted_changeset_returns_none_when_only_in_progress() {
        let t = chrono::Utc::now();
        let events = vec![make_event(
            1,
            "cs-0001",
            "agent-a",
            EventKind::TaskCreated {
                base_commit: GitOid::zero(),
                task: "task".into(),
            },
            t,
        )];
        let projection = Projection::from_events(&events);
        assert!(
            projection
                .latest_submitted_changeset(&AgentId("agent-a".into()))
                .is_none()
        );
    }

    #[test]
    fn latest_submitted_changeset_returns_submitted_for_correct_agent() {
        let t = chrono::Utc::now();
        let events = vec![
            // Agent A: overlay created then submitted
            make_event(
                1,
                "cs-0001",
                "agent-a",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: "task-a".into(),
                },
                t,
            ),
            make_event(
                2,
                "cs-0001",
                "agent-a",
                EventKind::ChangesetSubmitted { operations: vec![] },
                t,
            ),
            // Agent B: overlay created then submitted
            make_event(
                3,
                "cs-0002",
                "agent-b",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: "task-b".into(),
                },
                t,
            ),
            make_event(
                4,
                "cs-0002",
                "agent-b",
                EventKind::ChangesetSubmitted { operations: vec![] },
                t,
            ),
        ];
        let projection = Projection::from_events(&events);

        let result = projection.latest_submitted_changeset(&AgentId("agent-a".into()));
        assert!(result.is_some());
        assert_eq!(result.unwrap().id.0, "cs-0001");

        let result = projection.latest_submitted_changeset(&AgentId("agent-b".into()));
        assert!(result.is_some());
        assert_eq!(result.unwrap().id.0, "cs-0002");
    }

    #[test]
    fn latest_submitted_changeset_picks_most_recent_when_multiple_submitted() {
        let t1 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let t2 = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let events = vec![
            // First changeset: created at t1, submitted
            make_event(
                1,
                "cs-0001",
                "agent-a",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: "old task".into(),
                },
                t1,
            ),
            make_event(
                2,
                "cs-0001",
                "agent-a",
                EventKind::ChangesetSubmitted { operations: vec![] },
                t1,
            ),
            // Second changeset: created at t2, submitted (newer)
            make_event(
                3,
                "cs-0002",
                "agent-a",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: "new task".into(),
                },
                t2,
            ),
            make_event(
                4,
                "cs-0002",
                "agent-a",
                EventKind::ChangesetSubmitted { operations: vec![] },
                t2,
            ),
        ];
        let projection = Projection::from_events(&events);

        let result = projection
            .latest_submitted_changeset(&AgentId("agent-a".into()))
            .unwrap();
        assert_eq!(result.id.0, "cs-0002");
    }

    #[test]
    fn latest_submitted_changeset_skips_materialized_and_conflicted() {
        let t1 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let t2 = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let t3 = chrono::DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let events = vec![
            // cs-0001: submitted then materialized
            make_event(
                1,
                "cs-0001",
                "agent-a",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: "done".into(),
                },
                t1,
            ),
            make_event(
                2,
                "cs-0001",
                "agent-a",
                EventKind::ChangesetSubmitted { operations: vec![] },
                t1,
            ),
            make_event(
                3,
                "cs-0001",
                "agent-a",
                EventKind::ChangesetMaterialized {
                    new_commit: GitOid::zero(),
                },
                t1,
            ),
            // cs-0002: submitted (still pending) — this is the one we want
            make_event(
                4,
                "cs-0002",
                "agent-a",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: "pending".into(),
                },
                t2,
            ),
            make_event(
                5,
                "cs-0002",
                "agent-a",
                EventKind::ChangesetSubmitted { operations: vec![] },
                t2,
            ),
            // cs-0003: submitted then conflicted
            make_event(
                6,
                "cs-0003",
                "agent-a",
                EventKind::TaskCreated {
                    base_commit: GitOid::zero(),
                    task: "conflict".into(),
                },
                t3,
            ),
            make_event(
                7,
                "cs-0003",
                "agent-a",
                EventKind::ChangesetSubmitted { operations: vec![] },
                t3,
            ),
            make_event(
                8,
                "cs-0003",
                "agent-a",
                EventKind::ChangesetConflicted { conflicts: vec![] },
                t3,
            ),
        ];
        let projection = Projection::from_events(&events);

        let result = projection
            .latest_submitted_changeset(&AgentId("agent-a".into()))
            .unwrap();
        assert_eq!(result.id.0, "cs-0002");
    }
}
