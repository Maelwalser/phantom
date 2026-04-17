//! Derive an agent's submission context (changeset id, base commit, task)
//! from the event log.
//!
//! A single pass over the agent's events pulls out the most recent
//! `TaskCreated` record, then scans for a `ConflictResolutionStarted` that
//! updates the base commit after a resolution so a post-resolve submit no
//! longer re-detects the same conflict against a stale base.

use phantom_core::event::EventKind;
use phantom_core::id::{AgentId, ChangesetId, GitOid};
use phantom_core::traits::EventStore;

use crate::error::OrchestratorError;

/// Submission context extracted from an agent's event history.
#[derive(Debug)]
pub(super) struct AgentSubmissionContext {
    pub changeset_id: ChangesetId,
    pub base_commit: GitOid,
    pub task: String,
}

/// Resolve the submission context for `agent_id`.
///
/// Returns [`OrchestratorError::NotFound`] if the agent was never tasked (no
/// `TaskCreated` event in its history).
pub(super) async fn resolve_agent_context(
    events: &dyn EventStore,
    agent_id: &AgentId,
) -> Result<AgentSubmissionContext, OrchestratorError> {
    let agent_events = events
        .query_by_agent(agent_id)
        .await
        .map_err(|e| OrchestratorError::EventStore(e.to_string()))?;

    let (changeset_id, base_commit) = agent_events
        .iter()
        .rev()
        .find_map(|e| {
            if let EventKind::TaskCreated { base_commit, .. } = &e.kind {
                Some((e.changeset_id.clone(), *base_commit))
            } else {
                None
            }
        })
        .ok_or_else(|| {
            OrchestratorError::NotFound(format!(
                "no overlay found for agent '{agent_id}' — was it tasked?"
            ))
        })?;

    // If a conflict resolution updated the base, prefer the resolved base so a
    // post-resolution submit does not re-detect the same conflict.
    let base_commit = agent_events
        .iter()
        .rev()
        .find_map(|e| {
            if e.changeset_id == changeset_id
                && let EventKind::ConflictResolutionStarted {
                    new_base: Some(base),
                    ..
                } = &e.kind
            {
                return Some(*base);
            }
            None
        })
        .unwrap_or(base_commit);

    let task = agent_events
        .iter()
        .find_map(|e| {
            if let EventKind::TaskCreated { task, .. } = &e.kind {
                Some(task.clone())
            } else {
                None
            }
        })
        .unwrap_or_default();

    Ok(AgentSubmissionContext {
        changeset_id,
        base_commit,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;

    use phantom_core::conflict::ConflictDetail;
    use phantom_core::event::Event;
    use phantom_core::id::EventId;

    use crate::test_support::MockEventStore;

    fn event(agent: &str, cs: &str, kind: EventKind) -> Event {
        Event {
            id: EventId(0),
            timestamp: Utc::now(),
            changeset_id: ChangesetId(cs.into()),
            agent_id: AgentId(agent.into()),
            causal_parent: None,
            kind,
        }
    }

    #[tokio::test]
    async fn returns_not_found_when_agent_has_no_task_created() {
        let store = MockEventStore::new();
        let err = resolve_agent_context(&store, &AgentId("ghost".into()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, OrchestratorError::NotFound(_)),
            "expected NotFound, got: {err}"
        );
    }

    #[tokio::test]
    async fn extracts_task_and_base_from_task_created() {
        let store = MockEventStore::new();
        let base = GitOid::from_bytes([0x11; 20]);
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: base,
                    task: "refactor parser".into(),
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-a".into()))
            .await
            .unwrap();
        assert_eq!(ctx.changeset_id, ChangesetId("cs-1".into()));
        assert_eq!(ctx.base_commit, base);
        assert_eq!(ctx.task, "refactor parser");
    }

    #[tokio::test]
    async fn conflict_resolution_overrides_base_commit() {
        let store = MockEventStore::new();
        let original_base = GitOid::from_bytes([0x11; 20]);
        let resolved_base = GitOid::from_bytes([0x22; 20]);

        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: original_base,
                    task: "fix bug".into(),
                },
            ))
            .await
            .unwrap();
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::ConflictResolutionStarted {
                    conflicts: vec![] as Vec<ConflictDetail>,
                    new_base: Some(resolved_base),
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-a".into()))
            .await
            .unwrap();
        assert_eq!(ctx.base_commit, resolved_base, "resolved base must win");
        assert_eq!(ctx.changeset_id, ChangesetId("cs-1".into()));
    }

    #[tokio::test]
    async fn conflict_resolution_with_no_new_base_keeps_original() {
        let store = MockEventStore::new();
        let base = GitOid::from_bytes([0xAA; 20]);

        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: base,
                    task: String::new(),
                },
            ))
            .await
            .unwrap();
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::ConflictResolutionStarted {
                    conflicts: vec![] as Vec<ConflictDetail>,
                    new_base: None,
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-a".into()))
            .await
            .unwrap();
        assert_eq!(ctx.base_commit, base);
    }

    #[tokio::test]
    async fn conflict_resolution_for_other_changeset_is_ignored() {
        let store = MockEventStore::new();
        let base = GitOid::from_bytes([0xAA; 20]);
        let unrelated = GitOid::from_bytes([0xBB; 20]);

        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: base,
                    task: String::new(),
                },
            ))
            .await
            .unwrap();
        store
            .append(event(
                "agent-a",
                "cs-2",
                EventKind::ConflictResolutionStarted {
                    conflicts: vec![] as Vec<ConflictDetail>,
                    new_base: Some(unrelated),
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-a".into()))
            .await
            .unwrap();
        assert_eq!(
            ctx.base_commit, base,
            "a conflict resolution on a different changeset must not steal the base"
        );
    }
}
