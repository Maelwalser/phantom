//! Derive an agent's submission context (changeset id, base commit, task)
//! from the event log.
//!
//! A single pass over the agent's events pulls out the most recent
//! `TaskCreated` record, then scans for later events that advance the base:
//!
//! - `ConflictResolutionStarted { new_base: Some(_) }` — a resolve agent
//!   re-based the overlay after handling a conflict.
//! - `LiveRebased { new_base, .. }` — a successful ripple live-rebase
//!   silently pulled trunk changes into the overlay's upper layer.
//!
//! Both must advance the submission base; otherwise the submit pipeline
//! diffs the overlay against a stale base and fabricates `BothModifiedSymbol`
//! conflicts (and worse: attributes trunk-side changes the agent never touched
//! to the agent, which the materializer then tries to write back to trunk).
//! The latest of the two wins so a manual resolve after a silent rebase, or
//! vice versa, behaves correctly.

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

    // Prefer the most recent base advance within this changeset. Ripple-driven
    // `LiveRebased` and conflict-resolve-driven `ConflictResolutionStarted`
    // both advance the base; whichever came last is authoritative.
    //
    // `LiveRebased` only counts when EVERY shadowed file merged cleanly
    // (`conflicted_files` empty). A rebase that conflicted on some files
    // leaves those files at the *old* base content in the overlay's upper
    // layer; advancing the submission base past that would re-introduce the
    // exact false-positive bug in reverse — diffing old upper content
    // against a new base fabricates conflicts on every file trunk touched.
    // In the partially-rebased case we keep the previous base and let the
    // normal conflict flow handle the unmerged files.
    //
    // Scoping:
    // - `ConflictResolutionStarted` is scoped to the specific changeset
    //   being resolved, so it must match `changeset_id`.
    // - `LiveRebased` is emitted by ripple on the *other* agent's behalf,
    //   carrying the submitting agent's `changeset_id` but this agent's
    //   `agent_id`. It is still relevant to this agent's current changeset
    //   as long as it happened after the most recent `TaskCreated` — we
    //   already iterate only this agent's events, so any `LiveRebased`
    //   in the window since `TaskCreated` is authoritative regardless of
    //   which changeset triggered it.
    let task_created_idx = agent_events.iter().rposition(|e| {
        matches!(e.kind, EventKind::TaskCreated { .. }) && e.changeset_id == changeset_id
    });
    let base_commit = agent_events
        .iter()
        .enumerate()
        .rev()
        .take_while(|(i, _)| task_created_idx.is_none_or(|t| *i >= t))
        .find_map(|(_, e)| match &e.kind {
            EventKind::ConflictResolutionStarted {
                new_base: Some(base),
                ..
            } if e.changeset_id == changeset_id => Some(*base),
            EventKind::LiveRebased {
                new_base,
                conflicted_files,
                ..
            } if conflicted_files.is_empty() => Some(*new_base),
            _ => None,
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
    async fn live_rebase_advances_base_commit() {
        let store = MockEventStore::new();
        let original_base = GitOid::from_bytes([0x11; 20]);
        let rebased_base = GitOid::from_bytes([0x33; 20]);

        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: original_base,
                    task: "refactor".into(),
                },
            ))
            .await
            .unwrap();
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::LiveRebased {
                    old_base: original_base,
                    new_base: rebased_base,
                    merged_files: vec![],
                    conflicted_files: vec![],
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-a".into()))
            .await
            .unwrap();
        assert_eq!(
            ctx.base_commit, rebased_base,
            "a silent ripple live-rebase must advance the submission base"
        );
    }

    #[tokio::test]
    async fn live_rebase_with_conflicts_does_not_advance_base() {
        // When a ripple-driven live rebase conflicts on any file, that file's
        // overlay upper content is left at the OLD base. Advancing the
        // submission base would diff that old content against trunk HEAD and
        // fabricate conflicts on every other file trunk touched — the exact
        // symmetric failure of the bug this change fixes.
        let store = MockEventStore::new();
        let original_base = GitOid::from_bytes([0x11; 20]);
        let rebased_head = GitOid::from_bytes([0x22; 20]);

        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: original_base,
                    task: String::new(),
                },
            ))
            .await
            .unwrap();
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::LiveRebased {
                    old_base: original_base,
                    new_base: rebased_head,
                    merged_files: vec![],
                    conflicted_files: vec![std::path::PathBuf::from("src/lib.rs")],
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-a".into()))
            .await
            .unwrap();
        assert_eq!(
            ctx.base_commit, original_base,
            "a partially-rebased agent must keep its original base until resolve lands"
        );
    }

    #[tokio::test]
    async fn latest_of_live_rebase_or_resolution_wins() {
        let store = MockEventStore::new();
        let b0 = GitOid::from_bytes([0x10; 20]);
        let b1 = GitOid::from_bytes([0x11; 20]);
        let b2 = GitOid::from_bytes([0x22; 20]);
        let b3 = GitOid::from_bytes([0x33; 20]);

        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::TaskCreated {
                    base_commit: b0,
                    task: String::new(),
                },
            ))
            .await
            .unwrap();
        // Older resolve advance
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::ConflictResolutionStarted {
                    conflicts: vec![] as Vec<ConflictDetail>,
                    new_base: Some(b1),
                },
            ))
            .await
            .unwrap();
        // Then a live rebase
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::LiveRebased {
                    old_base: b1,
                    new_base: b2,
                    merged_files: vec![],
                    conflicted_files: vec![],
                },
            ))
            .await
            .unwrap();
        // Then another resolve advance
        store
            .append(event(
                "agent-a",
                "cs-1",
                EventKind::ConflictResolutionStarted {
                    conflicts: vec![] as Vec<ConflictDetail>,
                    new_base: Some(b3),
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-a".into()))
            .await
            .unwrap();
        assert_eq!(ctx.base_commit, b3, "latest base advance must win");
    }

    #[tokio::test]
    async fn live_rebase_from_ripple_is_picked_up_despite_changeset_id_mismatch() {
        // Cross-agent ripple: when agent A submits, agent B's overlay is live
        // rebased, and the resulting `LiveRebased` event is scoped to A's
        // submitting changeset (its `changeset_id`) even though the event's
        // `agent_id` is B. When B submits, discovery must pick up that
        // LiveRebased for B's base even though the changeset_id does not
        // match B's own changeset_id.
        let store = MockEventStore::new();
        let base = GitOid::from_bytes([0xAA; 20]);
        let ripple_new_base = GitOid::from_bytes([0xBB; 20]);

        store
            .append(event(
                "agent-b",
                "cs-b",
                EventKind::TaskCreated {
                    base_commit: base,
                    task: String::new(),
                },
            ))
            .await
            .unwrap();
        // LiveRebased carries agent-A's changeset_id but agent-B's agent_id.
        store
            .append(event(
                "agent-b",
                "cs-a-submitting",
                EventKind::LiveRebased {
                    old_base: base,
                    new_base: ripple_new_base,
                    merged_files: vec![std::path::PathBuf::from("src/lib.rs")],
                    conflicted_files: vec![],
                },
            ))
            .await
            .unwrap();

        let ctx = resolve_agent_context(&store, &AgentId("agent-b".into()))
            .await
            .unwrap();
        assert_eq!(
            ctx.base_commit, ripple_new_base,
            "a clean ripple-driven LiveRebased must advance the base even though changeset_id comes from the submitting agent"
        );
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
